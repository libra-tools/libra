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
    io::{Read, Write},
    panic,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use libra::{
    cli,
    internal::ai::authorized_read::{
        AUTHORIZED_READ_HELPER_ARG, AUTHORIZED_READ_HELPER_CAP_ENV, AUTHORIZED_READ_HELPER_MAX_CAP,
        AUTHORIZED_READ_HELPER_MODE_ENV,
    },
    utils::{
        error::INTERNAL_ERROR_REPORT_HINT,
        log_config::{LogRotation, resolve_log_config},
        output::OutputConfig,
    },
};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;

static STDOUT_BROKEN_PIPE_PANIC: AtomicBool = AtomicBool::new(false);
const REJECTED_CLEANUP_INDEX_HELPER_INPUT_CAP: u64 = 1024 * 1024;
const REJECTED_CLEANUP_INDEX_HELPER_OUTPUT_CAP: u64 = 64 * 1024 * 1024;

/// Outcome for a private helper stdin frame. The reader reserves no more than
/// the accepted content cap and probes overflow with one stack byte, avoiding
/// `read_to_end` growth when an exact-capacity frame reaches EOF.
enum StrictHelperStdinRead {
    Complete(Vec<u8>),
    Oversize { observed_bytes: u64 },
    Failed { bytes_read: u64 },
}

fn read_helper_stdin_strictly_bounded(cap: u64) -> StrictHelperStdinRead {
    let Ok(capacity) = usize::try_from(cap) else {
        return StrictHelperStdinRead::Failed { bytes_read: 0 };
    };
    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(capacity).is_err() || bytes.capacity() > capacity {
        return StrictHelperStdinRead::Failed { bytes_read: 0 };
    }

    let mut stdin = std::io::stdin().lock();
    let mut chunk = [0_u8; 8192];
    loop {
        let remaining = capacity.saturating_sub(bytes.len());
        if remaining == 0 {
            let mut sentinel = [0_u8; 1];
            loop {
                match stdin.read(&mut sentinel) {
                    Ok(0) => return StrictHelperStdinRead::Complete(bytes),
                    Ok(_) => {
                        return StrictHelperStdinRead::Oversize {
                            observed_bytes: cap.saturating_add(1),
                        };
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        return StrictHelperStdinRead::Failed {
                            bytes_read: bytes.len() as u64,
                        };
                    }
                }
            }
        }

        let read_len = remaining.min(chunk.len());
        match stdin.read(&mut chunk[..read_len]) {
            Ok(0) => return StrictHelperStdinRead::Complete(bytes),
            Ok(read) => bytes.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                return StrictHelperStdinRead::Failed {
                    bytes_read: bytes.len() as u64,
                };
            }
        }
    }
}

// Keep test fault controls out of ordinary debug binaries. `cfg!(debug_assertions)`
// is a runtime condition and remains true for `cargo build`, so it is not a
// production-safe substitute for `cfg(test)` here.
#[cfg(test)]
mod test_support {
    use std::{
        path::PathBuf,
        sync::{Mutex, OnceLock},
        time::Duration,
    };

    #[derive(Default)]
    pub(super) struct MainTestControls {
        pub(super) rejected_cleanup_index_helper_delay: Option<Duration>,
        pub(super) import_discovery_helper_delay: Option<Duration>,
        pub(super) subagent_discovery_helper_delay: Option<Duration>,
        pub(super) authorized_read_helper_delay: Option<Duration>,
        pub(super) authorized_read_helper_pid_file: Option<PathBuf>,
    }

    static CONTROLS: OnceLock<Mutex<MainTestControls>> = OnceLock::new();

    fn controls() -> &'static Mutex<MainTestControls> {
        CONTROLS.get_or_init(|| Mutex::new(MainTestControls::default()))
    }

    fn lock_controls() -> std::sync::MutexGuard<'static, MainTestControls> {
        controls()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) struct ControlsReset(Option<MainTestControls>);

    pub(super) fn install(controls: MainTestControls) -> ControlsReset {
        let previous = std::mem::replace(&mut *lock_controls(), controls);
        ControlsReset(Some(previous))
    }

    impl Drop for ControlsReset {
        fn drop(&mut self) {
            if let Some(previous) = self.0.take() {
                let _ = std::mem::replace(&mut *lock_controls(), previous);
            }
        }
    }

    pub(super) fn rejected_cleanup_index_helper_delay() -> Option<Duration> {
        lock_controls().rejected_cleanup_index_helper_delay
    }

    pub(super) fn import_discovery_helper_delay() -> Option<Duration> {
        lock_controls().import_discovery_helper_delay
    }

    pub(super) fn subagent_discovery_helper_delay() -> Option<Duration> {
        lock_controls().subagent_discovery_helper_delay
    }

    pub(super) fn authorized_read_helper_delay() -> Option<Duration> {
        lock_controls().authorized_read_helper_delay
    }

    pub(super) fn authorized_read_helper_pid_file() -> Option<PathBuf> {
        lock_controls().authorized_read_helper_pid_file.clone()
    }
}

fn run_checkpoint_object_io_helper_if_requested() -> Option<i32> {
    let mut args = std::env::args_os();
    let _program = args.next()?;
    if args.next()?.to_str() != Some(libra::internal::ai::history::CHECKPOINT_OBJECT_IO_HELPER_ARG)
        || args.next().is_some()
    {
        return None;
    }
    let StrictHelperStdinRead::Complete(input) = read_helper_stdin_strictly_bounded(
        libra::internal::ai::history::CHECKPOINT_OBJECT_IO_HELPER_INPUT_CAP,
    ) else {
        return Some(2);
    };
    let output = match libra::internal::ai::history::run_checkpoint_object_io_helper(&input) {
        Ok(output)
            if output.len() as u64
                <= libra::internal::ai::history::CHECKPOINT_OBJECT_IO_HELPER_OUTPUT_CAP =>
        {
            output
        }
        Ok(_) | Err(_) => return Some(2),
    };
    let mut stdout = std::io::stdout().lock();
    if stdout.write_all(&output).is_err() || stdout.flush().is_err() {
        return Some(1);
    }
    Some(0)
}

fn run_rejected_cleanup_index_helper_if_requested() -> Option<i32> {
    let mut args = std::env::args_os();
    let _program = args.next()?;
    if args.next()?.to_str()
        != Some(libra::internal::ai::history::REJECTED_CLEANUP_INDEX_HELPER_ARG)
        || args.next().is_some()
    {
        return None;
    }
    let StrictHelperStdinRead::Complete(input) =
        read_helper_stdin_strictly_bounded(REJECTED_CLEANUP_INDEX_HELPER_INPUT_CAP)
    else {
        return Some(2);
    };
    #[cfg(test)]
    if let Some(delay) = test_support::rejected_cleanup_index_helper_delay() {
        std::thread::sleep(delay);
    }
    let output = match libra::internal::ai::history::run_rejected_cleanup_index_helper(&input) {
        Ok(output) if output.len() as u64 <= REJECTED_CLEANUP_INDEX_HELPER_OUTPUT_CAP => output,
        Ok(_) | Err(_) => return Some(2),
    };
    let mut stdout = std::io::stdout().lock();
    if stdout.write_all(&output).is_err() || stdout.flush().is_err() {
        return Some(1);
    }
    Some(0)
}

fn run_import_discovery_helper_if_requested() -> Option<i32> {
    let mut args = std::env::args_os();
    let _program = args.next()?;
    if args.next()?.to_str() != Some(libra::command::agent::IMPORT_DISCOVERY_HELPER_ARG)
        || args.next().is_some()
    {
        return None;
    }
    #[cfg(test)]
    if let Some(delay) = test_support::import_discovery_helper_delay() {
        std::thread::sleep(delay);
    }
    let StrictHelperStdinRead::Complete(input) = read_helper_stdin_strictly_bounded(
        libra::command::agent::IMPORT_DISCOVERY_HELPER_FRAME_CAP,
    ) else {
        return Some(2);
    };
    let output = match libra::command::agent::run_import_discovery_helper(&input) {
        Ok(output)
            if output.len() as u64 <= libra::command::agent::IMPORT_DISCOVERY_HELPER_FRAME_CAP =>
        {
            output
        }
        Ok(_) | Err(_) => return Some(2),
    };
    let mut stdout = std::io::stdout().lock();
    if stdout.write_all(&output).is_err() || stdout.flush().is_err() {
        return Some(1);
    }
    Some(0)
}

fn run_import_preparation_descriptor_helper_if_requested() -> Option<i32> {
    let mut args = std::env::args_os();
    let _program = args.next()?;
    if args.next()?.to_str()
        != Some(libra::command::agent::IMPORT_PREPARATION_DESCRIPTOR_HELPER_ARG)
        || args.next().is_some()
    {
        return None;
    }
    let output = match libra::command::agent::run_import_preparation_descriptor_helper_from_stdin()
    {
        Ok(output)
            if output.len() as u64
                <= libra::command::agent::IMPORT_PREPARATION_HELPER_OUTPUT_CAP =>
        {
            output
        }
        Ok(_) | Err(_) => return Some(2),
    };
    let mut stdout = std::io::stdout().lock();
    if stdout.write_all(&output).is_err() || stdout.flush().is_err() {
        return Some(1);
    }
    Some(0)
}

fn run_import_index_repair_helper_if_requested() -> Option<i32> {
    let mut args = std::env::args_os();
    let _program = args.next()?;
    if args.next()?.to_str() != Some(libra::command::agent::IMPORT_INDEX_REPAIR_HELPER_ARG)
        || args.next().is_some()
    {
        return None;
    }
    let StrictHelperStdinRead::Complete(input) = read_helper_stdin_strictly_bounded(
        libra::command::agent::IMPORT_INDEX_REPAIR_HELPER_FRAME_CAP,
    ) else {
        return Some(2);
    };
    let output = match libra::command::agent::run_import_index_repair_helper(&input) {
        Ok(output)
            if output.len() as u64
                <= libra::command::agent::IMPORT_INDEX_REPAIR_HELPER_FRAME_CAP =>
        {
            output
        }
        Ok(_) | Err(_) => return Some(2),
    };
    let mut stdout = std::io::stdout().lock();
    if stdout.write_all(&output).is_err() || stdout.flush().is_err() {
        return Some(1);
    }
    Some(0)
}

fn run_subagent_discovery_helper_if_requested() -> Option<i32> {
    let mut args = std::env::args_os();
    let _program = args.next()?;
    if args.next()?.to_str()
        != Some(libra::internal::ai::subagent_content::SUBAGENT_DISCOVERY_HELPER_ARG)
        || args.next().is_some()
    {
        return None;
    }
    #[cfg(test)]
    if let Some(delay) = test_support::subagent_discovery_helper_delay() {
        std::thread::sleep(delay);
    }
    let StrictHelperStdinRead::Complete(input) = read_helper_stdin_strictly_bounded(
        libra::internal::ai::subagent_content::SUBAGENT_DISCOVERY_HELPER_INPUT_CAP,
    ) else {
        return Some(2);
    };
    let output = match libra::internal::ai::subagent_content::run_subagent_discovery_helper(&input)
    {
        Ok(output)
            if output.len() as u64
                <= libra::internal::ai::subagent_content::SUBAGENT_DISCOVERY_HELPER_OUTPUT_CAP =>
        {
            output
        }
        Ok(_) | Err(_) => return Some(2),
    };
    let mut stdout = std::io::stdout().lock();
    if stdout.write_all(&output).is_err() || stdout.flush().is_err() {
        return Some(1);
    }
    Some(0)
}

/// Killable subprocess boundary for CPU-heavy child-transcript projection.
/// It runs before logging and normal CLI initialization; stdin/stdout carry
/// only the bounded private binary frame consumed by the deadline owner.
fn run_subagent_projection_helper_if_requested() -> Option<i32> {
    let mut args = std::env::args_os();
    let _program = args.next()?;
    if args.next()?.to_str()
        != Some(libra::internal::ai::subagent_content::SUBAGENT_PROJECTION_HELPER_ARG)
        || args.next().is_some()
    {
        return None;
    }
    let StrictHelperStdinRead::Complete(input) = read_helper_stdin_strictly_bounded(
        libra::internal::ai::subagent_content::SUBAGENT_PROJECTION_HELPER_INPUT_CAP,
    ) else {
        return Some(2);
    };
    let output = match libra::internal::ai::subagent_content::run_subagent_projection_helper(input)
    {
        Ok(output)
            if output.len() as u64
                <= libra::internal::ai::subagent_content::SUBAGENT_PROJECTION_HELPER_OUTPUT_CAP =>
        {
            output
        }
        Ok(_) | Err(_) => return Some(2),
    };
    let mut stdout = std::io::stdout().lock();
    if stdout.write_all(&output).is_err() || stdout.flush().is_err() {
        return Some(1);
    }
    Some(0)
}

/// Killable subprocess boundary for redacted-only extraction metadata. The
/// helper receives no provider paths, raw errors, or native child bytes; it
/// runs before logging and normal CLI initialization so stdout stays a strict
/// private frame for the deadline-owning snapshot service.
fn run_capture_extraction_helper_if_requested() -> Option<i32> {
    let mut args = std::env::args_os();
    let _program = args.next()?;
    if args.next()?.to_str()
        != Some(libra::internal::ai::capture::snapshot::CAPTURE_EXTRACTION_HELPER_ARG)
        || args.next().is_some()
    {
        return None;
    }
    let StrictHelperStdinRead::Complete(input) = read_helper_stdin_strictly_bounded(
        libra::internal::ai::capture::snapshot::CAPTURE_EXTRACTION_HELPER_INPUT_CAP,
    ) else {
        return Some(2);
    };
    let output = libra::internal::ai::capture::snapshot::run_capture_extraction_helper(input);
    if output.len() as u64
        > libra::internal::ai::capture::snapshot::CAPTURE_EXTRACTION_HELPER_OUTPUT_CAP
    {
        return Some(2);
    }
    let mut stdout = std::io::stdout().lock();
    if stdout.write_all(&output).is_err() || stdout.flush().is_err() {
        return Some(1);
    }
    Some(0)
}

/// Killable status scan/probe I/O worker (plan-20260715 WIO-01). Handled
/// before CLI, upgrade, recovery, or any repository write. Stdin/stdout are
/// framed worker messages; the capability token is the only credential.
fn run_status_io_worker_if_requested() -> Option<i32> {
    let mut args = std::env::args_os();
    let _program = args.next()?;
    if args.next()?.to_str() != Some(libra::command::status_io_worker::STATUS_IO_WORKER_ARG)
        || args.next().is_some()
    {
        return None;
    }
    Some(libra::command::status_io_worker::run_worker())
}

/// Killable subprocess boundary for held-descriptor transcript reads. This is
/// handled before CLI/log initialization so stdout contains only the private
/// binary frame consumed by the parent importer.
fn run_authorized_read_helper_if_requested() -> Option<i32> {
    let mut args = std::env::args_os();
    let _program = args.next()?;
    if args.next()?.to_str() != Some(AUTHORIZED_READ_HELPER_ARG) || args.next().is_some() {
        return None;
    }
    let cap = match std::env::var(AUTHORIZED_READ_HELPER_CAP_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        Some(cap) if cap <= AUTHORIZED_READ_HELPER_MAX_CAP => cap,
        _ => return Some(2),
    };
    if std::env::var_os(AUTHORIZED_READ_HELPER_MODE_ENV)
        .is_some_and(|mode| mode == "live-claude-source-v2")
    {
        return Some(libra::internal::ai::authorized_read::run_live_claude_source_helper(cap));
    }
    if std::env::var_os(AUTHORIZED_READ_HELPER_MODE_ENV).is_some() {
        return Some(2);
    }
    #[cfg(test)]
    if let Some(path) = test_support::authorized_read_helper_pid_file()
        && std::fs::write(path, std::process::id().to_string()).is_err()
    {
        return Some(2);
    }
    #[cfg(test)]
    if let Some(delay) = test_support::authorized_read_helper_delay() {
        std::thread::sleep(delay);
    }

    let (status, raw_bytes, payload) = match read_helper_stdin_strictly_bounded(cap) {
        StrictHelperStdinRead::Complete(bytes) => (0u8, bytes.len() as u64, bytes),
        StrictHelperStdinRead::Oversize { observed_bytes } => (1u8, observed_bytes, Vec::new()),
        // The parent maps a failed helper read to a typed safe partial. Do
        // not send filesystem error text over this private raw-source frame.
        StrictHelperStdinRead::Failed { bytes_read } => (2u8, bytes_read, Vec::new()),
    };
    let mut stdout = std::io::stdout().lock();
    if stdout.write_all(&[status]).is_err()
        || stdout.write_all(&raw_bytes.to_le_bytes()).is_err()
        || stdout.write_all(&payload).is_err()
        || stdout.flush().is_err()
    {
        return Some(1);
    }
    Some(0)
}

/// Killable subprocess boundary for scope/worktree binding and the
/// repository-private capture replay key. This runs before tracing and normal
/// CLI startup; its sole fixed argument and bounded stdio frame keep it from
/// becoming a general command execution surface.
fn run_capture_scope_binding_helper_if_requested() -> Option<i32> {
    let mut args = std::env::args_os();
    let _program = args.next()?;
    if args.next()?.to_str()
        != Some(libra::internal::ai::hooks::runtime::CAPTURE_SCOPE_BINDING_HELPER_ARG)
        || args.next().is_some()
    {
        return None;
    }
    let StrictHelperStdinRead::Complete(input) = read_helper_stdin_strictly_bounded(
        libra::internal::ai::hooks::runtime::CAPTURE_SCOPE_BINDING_HELPER_INPUT_CAP,
    ) else {
        return Some(2);
    };
    let mut stdout = std::io::stdout().lock();
    if libra::internal::ai::hooks::runtime::run_capture_scope_binding_helper_to_writer(
        &input,
        &mut stdout,
    )
    .is_err()
    {
        return Some(1);
    }
    Some(0)
}

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
    libra::internal::ai::authorized_read::register_running_program();
    if let Some(exit_code) = run_checkpoint_object_io_helper_if_requested() {
        if exit_code != 0 {
            std::process::exit(exit_code);
        }
        return;
    }
    if let Some(exit_code) = run_rejected_cleanup_index_helper_if_requested() {
        if exit_code != 0 {
            std::process::exit(exit_code);
        }
        return;
    }
    if let Some(exit_code) = run_import_discovery_helper_if_requested() {
        if exit_code != 0 {
            std::process::exit(exit_code);
        }
        return;
    }
    if let Some(exit_code) = run_import_preparation_descriptor_helper_if_requested() {
        if exit_code != 0 {
            std::process::exit(exit_code);
        }
        return;
    }
    if let Some(exit_code) = run_import_index_repair_helper_if_requested() {
        if exit_code != 0 {
            std::process::exit(exit_code);
        }
        return;
    }
    if let Some(exit_code) = run_subagent_discovery_helper_if_requested() {
        if exit_code != 0 {
            std::process::exit(exit_code);
        }
        return;
    }
    if let Some(exit_code) = run_subagent_projection_helper_if_requested() {
        if exit_code != 0 {
            std::process::exit(exit_code);
        }
        return;
    }
    if let Some(exit_code) = run_capture_extraction_helper_if_requested() {
        if exit_code != 0 {
            std::process::exit(exit_code);
        }
        return;
    }
    if let Some(exit_code) = run_authorized_read_helper_if_requested() {
        if exit_code != 0 {
            std::process::exit(exit_code);
        }
        return;
    }
    if let Some(exit_code) = run_capture_scope_binding_helper_if_requested() {
        if exit_code != 0 {
            std::process::exit(exit_code);
        }
        return;
    }
    if let Some(exit_code) = run_status_io_worker_if_requested() {
        std::process::exit(exit_code);
    }
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
        // `args_os`, not `args`: the latter PANICS on an argument that is
        // not valid UTF-8, and this runs on the ERROR path — aborting here
        // would replace a clean diagnostic with a panic. Tokens that are not
        // UTF-8 cannot be the ASCII output flags this resolves, so they are
        // dropped rather than lossily transcoded.
        let argv: Vec<String> = std::env::args_os()
            .filter_map(|arg| arg.into_string().ok())
            .collect();
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
            // INVARIANT: this static directive is accepted by all supported
            // tracing-subscriber versions; it is not derived from user input.
            .expect("static rfuse3 directive must parse"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_fault_controls_are_in_process_test_state() {
        let directory = tempfile::tempdir().expect("create main test-control directory");
        let pid_file = directory.path().join("authorized-read.pid");
        let _reset = test_support::install(test_support::MainTestControls {
            rejected_cleanup_index_helper_delay: Some(std::time::Duration::from_millis(1)),
            import_discovery_helper_delay: Some(std::time::Duration::from_millis(2)),
            subagent_discovery_helper_delay: Some(std::time::Duration::from_millis(3)),
            authorized_read_helper_delay: Some(std::time::Duration::from_millis(4)),
            authorized_read_helper_pid_file: Some(pid_file.clone()),
        });

        assert_eq!(
            test_support::rejected_cleanup_index_helper_delay(),
            Some(std::time::Duration::from_millis(1))
        );
        assert_eq!(
            test_support::import_discovery_helper_delay(),
            Some(std::time::Duration::from_millis(2))
        );
        assert_eq!(
            test_support::subagent_discovery_helper_delay(),
            Some(std::time::Duration::from_millis(3))
        );
        assert_eq!(
            test_support::authorized_read_helper_delay(),
            Some(std::time::Duration::from_millis(4))
        );
        assert_eq!(
            test_support::authorized_read_helper_pid_file(),
            Some(pid_file)
        );
    }
}
