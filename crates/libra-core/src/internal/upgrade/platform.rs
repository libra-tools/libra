//! Release platform matrix for the auto-upgrade subsystem (plan-20260714
//! §A.1/§A.6).
//!
//! The manifest's `artifacts` must cover this exact matrix, and a client only
//! ever selects its own platform. Windows is published but explicitly
//! unsupported for auto-upgrade in the first phase (§A.1): selecting it
//! reports [`PlatformSupport::Unsupported`] before any download path runs.

use std::fmt;

/// Platforms present in the official release matrix (`release.yml`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    /// `libra-linux-amd64`
    LinuxAmd64,
    /// `libra-linux-arm64`
    LinuxArm64,
    /// `libra-darwin-arm64`
    DarwinArm64,
    /// `libra-windows-amd64` (published; auto-upgrade unsupported in R0)
    WindowsAmd64,
}

/// Whether auto-upgrade may proceed on this platform (§A.1 support matrix).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformSupport {
    /// Auto-upgrade fully supported.
    Supported,
    /// Published platform, but auto-upgrade must return `UnsupportedPlatform`
    /// and leave the installed binary untouched (Windows in R0).
    Unsupported,
}

impl Platform {
    /// Every platform the release matrix publishes, in canonical order.
    pub const RELEASE_MATRIX: &[Platform] = &[
        Platform::LinuxAmd64,
        Platform::LinuxArm64,
        Platform::DarwinArm64,
        Platform::WindowsAmd64,
    ];

    /// The manifest/platform identifier (also the URL artifact suffix:
    /// `libra-{platform}`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LinuxAmd64 => "linux-amd64",
            Self::LinuxArm64 => "linux-arm64",
            Self::DarwinArm64 => "darwin-arm64",
            Self::WindowsAmd64 => "windows-amd64",
        }
    }

    /// Parse a manifest platform identifier.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "linux-amd64" => Some(Self::LinuxAmd64),
            "linux-arm64" => Some(Self::LinuxArm64),
            "darwin-arm64" => Some(Self::DarwinArm64),
            "windows-amd64" => Some(Self::WindowsAmd64),
            _ => None,
        }
    }

    /// §A.1 first-phase support matrix.
    pub fn support(self) -> PlatformSupport {
        match self {
            Self::LinuxAmd64 | Self::LinuxArm64 | Self::DarwinArm64 => PlatformSupport::Supported,
            Self::WindowsAmd64 => PlatformSupport::Unsupported,
        }
    }

    /// The platform of the running binary, when it is part of the release
    /// matrix at all (e.g. macOS x86_64 is not published and returns `None`,
    /// §A.1).
    pub fn current() -> Option<Self> {
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            Some(Self::LinuxAmd64)
        }
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        {
            Some(Self::LinuxArm64)
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            Some(Self::DarwinArm64)
        }
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        {
            Some(Self::WindowsAmd64)
        }
        #[cfg(not(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "windows", target_arch = "x86_64"),
        )))]
        {
            None
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_round_trips_and_is_unique() {
        let mut seen = std::collections::HashSet::new();
        for p in Platform::RELEASE_MATRIX {
            assert_eq!(Platform::parse(p.as_str()), Some(*p));
            assert!(seen.insert(p.as_str()), "duplicate platform id");
        }
        assert_eq!(Platform::parse("darwin-amd64"), None, "not in the matrix");
    }

    #[test]
    fn windows_is_published_but_unsupported() {
        assert_eq!(
            Platform::WindowsAmd64.support(),
            PlatformSupport::Unsupported
        );
        assert_eq!(Platform::LinuxAmd64.support(), PlatformSupport::Supported);
    }
}
