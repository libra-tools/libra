//! Shared pager policy for CLI commands.
//!
//! Commands that want paged output should go through this module instead of
//! spawning `less` directly. That keeps test runs non-interactive by default
//! while still allowing explicit pager-path coverage via `LIBRA_PAGER=always`.

#[cfg(unix)]
use std::process::{Child, Command, Stdio};
use std::{
    env,
    io::{self, IsTerminal, Write},
};

use crate::utils::{
    error::{CliError, CliResult, StableErrorCode},
    output::OutputConfig,
};

pub const LIBRA_PAGER_ENV: &str = "LIBRA_PAGER";
pub const LIBRA_TEST_ENV: &str = "LIBRA_TEST";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PagerMode {
    Auto,
    Always,
    Never,
}

impl PagerMode {
    fn from_env() -> Self {
        match env::var(LIBRA_PAGER_ENV) {
            Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
                "always" => Self::Always,
                "never" => Self::Never,
                _ => Self::Auto,
            },
            Err(_) => Self::Auto,
        }
    }
}

#[derive(Debug)]
enum OutputTarget {
    Stdout(io::Stdout),
    #[cfg(unix)]
    Pager(Child),
    Finished,
}

/// Writer that targets either stdout or a pager process.
#[derive(Debug)]
pub struct Pager {
    target: OutputTarget,
    closed: bool,
}

impl Pager {
    /// Create a writer that pages output when policy allows it.
    pub fn new() -> CliResult<Self> {
        let mode = PagerMode::from_env();

        match mode {
            PagerMode::Never => Ok(Self::stdout()),
            PagerMode::Always => Self::spawn_pager(),
            PagerMode::Auto => {
                if should_use_pager() {
                    Self::spawn_pager().or_else(|_| Ok(Self::stdout()))
                } else {
                    Ok(Self::stdout())
                }
            }
        }
    }

    /// Create a writer that respects the global [`OutputConfig`].
    ///
    /// Returns plain stdout when the config disables paging (`--no-pager`,
    /// `--machine`, or `--json` mode).  Otherwise delegates to `Pager::new()`.
    pub fn with_config(config: &OutputConfig) -> CliResult<Self> {
        if !config.pager || config.is_json() {
            Ok(Self::stdout())
        } else {
            Self::new()
        }
    }

    fn stdout() -> Self {
        Self {
            target: OutputTarget::Stdout(io::stdout()),
            closed: false,
        }
    }

    #[cfg(unix)]
    fn spawn_pager() -> CliResult<Self> {
        let child = Command::new("less")
            .arg("-R")
            .arg("-F")
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .spawn()
            .map_err(pager_spawn_error)?;
        Ok(Self {
            target: OutputTarget::Pager(child),
            closed: false,
        })
    }

    #[cfg(not(unix))]
    fn spawn_pager() -> CliResult<Self> {
        Ok(Self::stdout())
    }

    pub fn write_str(&mut self, text: &str) -> CliResult<()> {
        self.write_all(text.as_bytes())
    }

    pub fn write_line(&mut self, line: &str) -> CliResult<()> {
        self.write_str(line)?;
        self.write_all(b"\n")
    }

    pub fn finish(mut self) -> CliResult<()> {
        let target = std::mem::replace(&mut self.target, OutputTarget::Finished);

        if self.closed {
            #[cfg(unix)]
            if let OutputTarget::Pager(mut child) = target {
                let _ = child.stdin.take();
                let _ = child.wait();
            }
            return Ok(());
        }

        match target {
            OutputTarget::Stdout(mut stdout) => match stdout.flush() {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(()),
                Err(err) => Err(pager_io_error("flush stdout", err)),
            },
            #[cfg(unix)]
            OutputTarget::Pager(mut child) => {
                let _ = child.stdin.take();
                child
                    .wait()
                    .map(|_| ())
                    .map_err(|err| pager_io_error("wait for pager", err))
            }
            OutputTarget::Finished => Ok(()),
        }
    }

    fn write_all(&mut self, bytes: &[u8]) -> CliResult<()> {
        if self.closed {
            return Ok(());
        }

        let result = match &mut self.target {
            OutputTarget::Stdout(stdout) => stdout.write_all(bytes),
            #[cfg(unix)]
            OutputTarget::Pager(child) => {
                let stdin = child
                    .stdin
                    .as_mut()
                    .ok_or_else(|| CliError::fatal("failed to capture pager stdin"))?;
                stdin.write_all(bytes)
            }
            OutputTarget::Finished => Ok(()),
        };

        match result {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {
                self.closed = true;
                Ok(())
            }
            Err(err) => Err(pager_io_error("write output", err)),
        }
    }
}

fn should_use_pager() -> bool {
    // This terminal check runs before we spawn the pager process. Once paging is
    // enabled, the command writes to the pager's stdin pipe instead of directly
    // to the user's terminal.
    io::stdout().is_terminal() && env::var_os(LIBRA_TEST_ENV).is_none()
}

#[cfg(unix)]
fn pager_spawn_error(err: io::Error) -> CliError {
    CliError::fatal(format!("failed to execute pager: {err}"))
        .with_stable_code(StableErrorCode::IoWriteFailed)
}

fn pager_io_error(action: &str, err: io::Error) -> CliError {
    CliError::fatal(format!("failed to {action}: {err}"))
        .with_stable_code(StableErrorCode::IoWriteFailed)
}

impl Drop for Pager {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let OutputTarget::Pager(child) = &mut self.target {
            let _ = child.stdin.take();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use serial_test::serial;

    use super::{LIBRA_PAGER_ENV, LIBRA_TEST_ENV, PagerMode, should_use_pager};

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: these tests run under `serial` and do not share env access.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: these tests run under `serial` and restore the prior env.
            unsafe {
                if let Some(value) = &self.previous {
                    std::env::set_var(self.key, value);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    #[test]
    #[serial(env)]
    fn pager_mode_defaults_to_auto() {
        let _pager = EnvGuard::set(LIBRA_PAGER_ENV, "unexpected-value");
        assert_eq!(PagerMode::from_env(), PagerMode::Auto);
    }

    #[test]
    #[serial(env)]
    fn libra_test_disables_auto_pager() {
        let _test = EnvGuard::set(LIBRA_TEST_ENV, "1");
        let _pager = EnvGuard::set(LIBRA_PAGER_ENV, "auto");
        assert!(!should_use_pager());
    }
}
