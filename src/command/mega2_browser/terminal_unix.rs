//! Unix terminal lifecycle via direct `libc` termios (no third-party crates).

use std::io::{self, Write};

use crate::utils::error::{CliError, CliResult, StableErrorCode};

const ENTER_ALT_SCREEN: &[u8] = b"\x1b[?1049h\x1b[?25l";
const LEAVE_ALT_SCREEN: &[u8] = b"\x1b[?1049l\x1b[?25h";

/// Raw-mode + alternate-screen guard. Restores termios and the main screen on
/// drop (RAII), including panic unwind.
pub struct UnixTerminalGuard {
    fd: libc::c_int,
    saved: libc::termios,
    restored: bool,
}

impl UnixTerminalGuard {
    pub fn enter() -> CliResult<Self> {
        Self::enter_on_fd(libc::STDIN_FILENO)
    }

    /// Test seam: same lifecycle on an arbitrary terminal fd (e.g. a pty slave),
    /// so restoration is verifiable without hijacking the process stdin.
    pub(crate) fn enter_on_fd(fd: libc::c_int) -> CliResult<Self> {
        // SAFETY: zeroed termios is a valid initial value for tcgetattr output.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: caller-provided fd is a valid terminal descriptor.
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            return Err(CliError::fatal(format!(
                "mega2 browser: cannot read terminal settings: {}",
                io::Error::last_os_error()
            ))
            .with_stable_code(StableErrorCode::IoReadFailed));
        }

        let mut raw = saved;
        raw.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG | libc::IEXTEN);
        raw.c_iflag &= !(libc::IXON | libc::ICRNL | libc::BRKINT | libc::INPCK | libc::ISTRIP);
        raw.c_oflag &= !(libc::OPOST);
        raw.c_cflag |= libc::CS8;
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: raw was initialized from saved and only flag bits changed.
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &raw) } != 0 {
            return Err(CliError::fatal(format!(
                "mega2 browser: cannot enter raw mode: {}",
                io::Error::last_os_error()
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed));
        }

        let guard = Self {
            fd,
            saved,
            restored: false,
        };
        guard.write_all(ENTER_ALT_SCREEN).map_err(|e| {
            CliError::fatal(format!("mega2 browser: cannot enter alternate screen: {e}"))
                .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
        Ok(guard)
    }

    fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        let mut stdout = io::stdout().lock();
        stdout.write_all(bytes)?;
        stdout.flush()
    }

    /// Best-effort restore, idempotent. Keeps the first failure context visible
    /// but never panics.
    pub fn restore(&mut self) -> io::Result<()> {
        let mut first_error = None;
        if !self.restored {
            self.restored = true;
            if self.write_all(LEAVE_ALT_SCREEN).is_err() {
                first_error = Some(io::Error::other("failed to leave the alternate screen"));
            }
            // SAFETY: `saved` holds the termios captured before entering raw mode.
            if unsafe { libc::tcsetattr(self.fd, libc::TCSAFLUSH, &self.saved) } != 0
                && first_error.is_none()
            {
                first_error = Some(io::Error::last_os_error());
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl Drop for UnixTerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_mode_changes_and_restores_termios_on_a_pty() {
        // SAFETY: openpty initializes the provided fd storage; termios is null
        // meaning defaults are used.
        let mut master: libc::c_int = -1;
        let mut slave: libc::c_int = -1;
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if rc != 0 {
            // ptys unavailable in this environment; nothing to verify.
            return;
        }
        // SAFETY: fd is a valid slave terminal open for the whole test.
        let mut before: libc::termios = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::tcgetattr(slave, &mut before) }, 0);

        {
            let guard = UnixTerminalGuard::enter_on_fd(slave).expect("enter raw mode");
            let mut raw: libc::termios = unsafe { std::mem::zeroed() };
            assert_eq!(unsafe { libc::tcgetattr(slave, &mut raw) }, 0);
            assert_eq!(raw.c_lflag & libc::ICANON, 0, "ICANON disabled");
            assert_eq!(raw.c_lflag & libc::ECHO, 0, "ECHO disabled");
            drop(guard);
        }

        let mut after: libc::termios = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::tcgetattr(slave, &mut after) }, 0);
        assert_eq!(after.c_lflag, before.c_lflag, "termios flags restored");
        assert_eq!(after.c_iflag, before.c_iflag, "input flags restored");

        // SAFETY: descriptors were opened by openpty above.
        unsafe {
            libc::close(slave);
            libc::close(master);
        }
    }
}
