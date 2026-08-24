//! Library entry for the Libra CLI.
//!
//! This crate has two faces:
//! 1. The `libra` binary (see `main.rs`) parses the process argv and dispatches to
//!    [`cli::parse`].
//! 2. Embedders (integration tests, the Web Code UI, and external Rust crates that drive
//!    Libra programmatically) call [`exec`] or [`exec_async`] with a pre-built argv.
//!
//! The supported, patch-compatible embedding API is [`exec`], [`exec_async`],
//! and the public `Cli*` result/error types re-exported below. The public module
//! tree exists for the binary, integration tests, and in-tree adapters; in
//! particular, `internal` is an implementation detail and is not a stable
//! external embedding surface.

// The bridge dispatcher (`command::agent::bridge`) awaits the full VCS service
// stack inside one async block, whose type layout nests deeper than rustc's
// default query depth. Raising the limit is a compile-time-only knob.
#![recursion_limit = "256"]

pub mod cli;
pub mod command;
pub mod common_utils;
pub mod git_protocol;
pub mod internal;
pub mod lfs_structs;
pub mod utils;

pub use utils::error::{CliError, CliErrorKind, CliResult};

/// Execute a Libra command synchronously.
///
/// Functional scope:
/// - Prepends the binary name (`libra`) to `args` so callers can use the same
///   "args without argv\[0\]" convention as `std::process::Command`.
/// - Spins up a private multi-thread Tokio runtime, blocks on the async dispatcher,
///   and returns the dispatcher's `CliResult` unchanged.
///
/// Boundary conditions:
/// - **Caution:** This function creates its own Tokio runtime. Calling it from within
///   an existing Tokio runtime panics because Tokio runtimes cannot be nested. From
///   async code, call [`exec_async`] instead.
/// - The caller's `Vec<&str>` is consumed (mutated by the `insert`); pass a clone if
///   the original must be preserved.
///
/// Examples:
/// - `["init"]`
/// - `["add", "."]`
pub fn exec(mut args: Vec<&str>) -> CliResult<()> {
    args.insert(0, env!("CARGO_PKG_NAME"));
    cli::parse(Some(&args))
}

/// Async counterpart of [`exec`].
///
/// Functional scope:
/// - Uses the caller's existing Tokio runtime — safe to await from any async context.
/// - Prepends the binary name to `args`, then forwards to [`cli::parse_async`].
///
/// Boundary conditions:
/// - Errors from any subcommand bubble up via `CliResult::Err`; the function does not
///   print them itself, leaving error rendering to the caller (typically `main.rs`).
/// - Concurrent calls in one process are serialized because CLI dispatch mutates
///   process-global CWD/hash/output state and drains one shared object-index queue.
///   Callers may await concurrently, but commands execute one at a time.
pub async fn exec_async(mut args: Vec<&str>) -> CliResult<()> {
    args.insert(0, env!("CARGO_PKG_NAME"));
    Box::pin(async move { cli::parse_async(Some(&args)).await }).await
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use serial_test::serial;
    use tempfile::TempDir;

    use crate::utils::test::{self, ScopedEnvVar};

    /// Smoke test: verifies that the [`ChangeDirGuard`](test::ChangeDirGuard) test
    /// helper can be acquired against a freshly-created temporary directory.
    ///
    /// Scenario: this guard is the foundation of every test that mutates the process
    /// CWD. If the guard cannot construct, every other test in the suite is unsafe to
    /// run, so we exercise the happy path here as a canary.
    #[test]
    #[serial]
    fn test_libra_init() {
        let tmp_dir = TempDir::new().unwrap();
        let _guard = test::ChangeDirGuard::new(tmp_dir.path());
    }

    #[test]
    #[serial]
    fn exec_async_object_index_drain_yields_current_thread_executor() {
        std::thread::Builder::new()
            .name("exec-async-drain-test".to_string())
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build current-thread test runtime")
                    .block_on(async {
                        let repo = TempDir::new().expect("create async exec repository");
                        test::setup_with_new_libra_in(repo.path()).await;
                        let _cwd = test::ChangeDirGuard::new(repo.path());
                        let input = repo.path().join("payload.txt");
                        fs::write(&input, b"async drain payload").expect("write hash-object input");
                        let _delay =
                            ScopedEnvVar::set("LIBRA_TEST_OBJECT_INDEX_UPDATE_DELAY_MS", "500");

                        let timer_fired = Arc::new(AtomicBool::new(false));
                        let timer_state = Arc::clone(&timer_fired);
                        let timer = tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            timer_state.store(true, Ordering::SeqCst);
                        });
                        let input_arg = input.to_string_lossy().into_owned();
                        super::exec_async(vec!["hash-object", "-w", input_arg.as_str()])
                            .await
                            .expect("embedded hash-object succeeds");
                        assert!(
                            timer_fired.load(Ordering::SeqCst),
                            "object-index drain blocked the caller's current-thread Tokio executor"
                        );
                        timer.await.expect("join executor responsiveness timer");
                    });
            })
            .expect("spawn large-stack async exec test")
            .join()
            .expect("join async exec test");
    }
}
