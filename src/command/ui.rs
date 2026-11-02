//! `libra ui` — open the desktop window.
//!
//! The window itself runs on the process main thread (`gui::run`). This module
//! is the clap surface and the refusal used when the library dispatcher is
//! reached instead of the process entry point.

use clap::Parser;

use crate::utils::error::{CliError, CliResult, StableErrorCode};

pub const UI_EXAMPLES: &str = "\
EXAMPLES:
    libra ui        Open the desktop window";

/// Open the Libra desktop window.
#[derive(Parser, Debug)]
#[command(after_help = UI_EXAMPLES)]
pub struct UiArgs {}

/// Why `libra ui` did not open a window from the dispatcher.
///
/// The process entry point handles a bare `libra ui` before this runs.
/// Reaching here means the caller is not the process entry point (for
/// example `libra::exec`), which cannot hand the main thread to GPUI.
pub fn unavailable_error() -> CliError {
    CliError::fatal(
        "`libra ui` opens its window on the process main thread. Run the `libra` executable directly (`libra ui`). The library dispatcher cannot open it.",
    )
    .with_stable_code(StableErrorCode::Unsupported)
}

/// Why `libra ui` cannot open a window because Linux has no display.
pub fn display_unavailable_error() -> CliError {
    CliError::fatal(
        "`libra ui` needs a graphical display. Set DISPLAY or WAYLAND_DISPLAY and try again.",
    )
    .with_stable_code(StableErrorCode::Unsupported)
    .with_hint("run it from a desktop session, or use the CLI commands for headless automation")
}

/// Dispatcher entry. Never opens a window; see [`unavailable_error`].
pub fn execute(_args: UiArgs) -> CliResult<()> {
    Err(unavailable_error())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_error_explains_the_main_thread_requirement() {
        let error = unavailable_error();
        let message = error.render();
        assert!(
            message.contains("main thread"),
            "the dispatcher refusal should say why it cannot open a window, got: {message}"
        );
        assert_eq!(error.stable_code().as_str(), "LBR-UNSUPPORTED-001");
    }

    #[test]
    fn display_error_names_the_missing_display() {
        let error = display_unavailable_error();
        let message = error.render();
        assert!(
            message.contains("DISPLAY") && message.contains("WAYLAND_DISPLAY"),
            "the headless refusal should name the display variables, got: {message}"
        );
        assert_eq!(error.stable_code().as_str(), "LBR-UNSUPPORTED-001");
    }
}
