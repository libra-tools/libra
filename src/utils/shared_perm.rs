//! Shared repository permissions helper aligning with Git's `path.c:calc_shared_perm`.

use std::{fs, io, path::Path};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SharedModeError {
    #[error("invalid shared mode '{0}': file owner must have read and write permissions")]
    OwnerPermissionsMissing(String),
    #[error("invalid shared mode '{0}'")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SharedMode {
    Umask,
    Group,
    All,
    Numeric { raw: String, bits: u32 },
}

impl SharedMode {
    pub fn parse(raw: &str) -> Result<Self, SharedModeError> {
        let trimmed = raw.trim();
        let lower = trimmed.to_ascii_lowercase();
        match lower.as_str() {
            "umask" | "false" | "no" | "off" | "0" => Ok(Self::Umask),
            "group" | "true" | "yes" | "on" | "1" => Ok(Self::Group),
            "all" | "world" | "everybody" | "2" => Ok(Self::All),
            _ => {
                if let Ok(val) = u32::from_str_radix(trimmed, 8) {
                    if val == 0 {
                        return Ok(Self::Umask);
                    }
                    if val == 1 {
                        return Ok(Self::Group);
                    }
                    if val == 2 {
                        return Ok(Self::All);
                    }
                    if val & 0o600 != 0o600 {
                        return Err(SharedModeError::OwnerPermissionsMissing(raw.to_string()));
                    }
                    let bits = val & 0o666;
                    return Ok(Self::Numeric {
                        raw: format!("0{bits:o}"),
                        bits,
                    });
                }
                Err(SharedModeError::Invalid(raw.to_string()))
            }
        }
    }

    pub fn config_value(&self) -> &str {
        match self {
            Self::Umask => "0",
            Self::Group => "1",
            Self::All => "2",
            Self::Numeric { raw, .. } => raw.as_str(),
        }
    }

    pub fn should_persist(&self) -> bool {
        !matches!(self, Self::Umask)
    }

    #[cfg(not(target_os = "windows"))]
    pub fn permission_bits(&self) -> Option<u32> {
        match self {
            Self::Umask => None,
            Self::Group => Some(0o2775),
            Self::All => Some(0o2777),
            Self::Numeric { bits, .. } => Some(*bits),
        }
    }
}

/// Calculate the resulting file or directory mode for `mode` given `shared`.
///
/// Follows Git's `calc_shared_perm` in `path.c`:
/// - When `shared` is `Umask`, returns `mode` unchanged.
/// - For non-numeric shared modes:
///   - Group: overlay `0660` (or `0664` for everybody / All)
///   - If owner cannot write, strip group write (`!(mode & 0200) -> mode &= ~0020`).
///   - If owner can execute, copy read bits to execute bits (`mode |= (mode & 0444) >> 2`).
/// - For numeric modes: replace lower 9 bits (`mode &= !0777; mode |= (bits & 0777);`).
/// - For directories:
///   - Copy read bits to execute bits (`mode |= (mode & 0444) >> 2`).
///   - If `Group`, `All`, or numeric mode with group read/write (`bits & 0060 != 0`), add setgid (`02000`).
pub fn calc_shared_perm(mode: u32, is_dir: bool, shared: &SharedMode) -> u32 {
    let mut perm = mode;

    match shared {
        SharedMode::Umask => return mode,
        SharedMode::Group => {
            perm |= 0o660;
            if (perm & 0o200) == 0 {
                perm &= !0o020;
            }
            if (perm & 0o100) != 0 {
                perm |= (perm & 0o444) >> 2;
            }
        }
        SharedMode::All => {
            perm |= 0o664;
            if (perm & 0o200) == 0 {
                perm &= !0o020;
            }
            if (perm & 0o100) != 0 {
                perm |= (perm & 0o444) >> 2;
            }
        }
        SharedMode::Numeric { bits, .. } => {
            perm = (perm & !0o777) | (bits & 0o777);
        }
    }

    if is_dir {
        perm |= (perm & 0o444) >> 2;
        let has_group = match shared {
            SharedMode::Group | SharedMode::All => true,
            SharedMode::Numeric { bits, .. } => (bits & 0o060) != 0,
            SharedMode::Umask => false,
        };
        if has_group {
            perm |= 0o2000;
        }
    }

    perm
}

/// Adjust filesystem permissions of `path` based on `shared`.
///
/// On Windows, this is an explicit no-op. On Unix, this applies `calc_shared_perm`
/// without following symlinks.
#[cfg(unix)]
pub fn adjust_shared_perm(path: &Path, shared: &SharedMode) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if matches!(shared, SharedMode::Umask) {
        return Ok(());
    }

    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }

    let is_dir = metadata.is_dir();
    let current_mode = metadata.permissions().mode();
    let target_mode = calc_shared_perm(current_mode, is_dir, shared);

    if (current_mode & 0o7777) != (target_mode & 0o7777) {
        let mut perms = metadata.permissions();
        perms.set_mode(target_mode);
        fs::set_permissions(path, perms)?;
    }

    Ok(())
}

#[cfg(not(unix))]
pub fn adjust_shared_perm(_path: &Path, _shared: &SharedMode) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calc_shared_perm_table() {
        // Group mode (shared == PERM_GROUP in Git)
        let group = SharedMode::Group;
        // Non-executable regular file (0644 -> 0664)
        assert_eq!(calc_shared_perm(0o644, false, &group), 0o664);
        assert_eq!(calc_shared_perm(0o600, false, &group), 0o660);
        // Executable regular file (0755 -> 0775)
        assert_eq!(calc_shared_perm(0o755, false, &group), 0o775);
        // Read-only regular file (0444 -> 0664)
        assert_eq!(calc_shared_perm(0o444, false, &group), 0o664);
        // Directory (0755 -> 02775)
        assert_eq!(calc_shared_perm(0o755, true, &group), 0o2775);
        assert_eq!(calc_shared_perm(0o700, true, &group), 0o2770);

        // All mode (shared == PERM_EVERYBODY in Git)
        let all = SharedMode::All;
        assert_eq!(calc_shared_perm(0o644, false, &all), 0o664);
        assert_eq!(calc_shared_perm(0o755, false, &all), 0o775);
        assert_eq!(calc_shared_perm(0o755, true, &all), 0o2775);

        // Numeric modes (0660 -> dir 02770, file 0660)
        let num_660 = SharedMode::Numeric {
            raw: "0660".to_string(),
            bits: 0o660,
        };
        assert_eq!(calc_shared_perm(0o644, false, &num_660), 0o660);
        assert_eq!(calc_shared_perm(0o755, true, &num_660), 0o2770);

        // Numeric modes (0640 -> dir 02750, file 0640)
        let num_640 = SharedMode::Numeric {
            raw: "0640".to_string(),
            bits: 0o640,
        };
        assert_eq!(calc_shared_perm(0o644, false, &num_640), 0o640);
        assert_eq!(calc_shared_perm(0o755, true, &num_640), 0o2750);

        // Numeric modes (0600 -> dir 0700, file 0600)
        let num_600 = SharedMode::Numeric {
            raw: "0600".to_string(),
            bits: 0o600,
        };
        assert_eq!(calc_shared_perm(0o644, false, &num_600), 0o600);
        assert_eq!(calc_shared_perm(0o755, true, &num_600), 0o700);

        // Umask leaves modes unchanged
        let umask = SharedMode::Umask;
        assert_eq!(calc_shared_perm(0o644, false, &umask), 0o644);
        assert_eq!(calc_shared_perm(0o755, true, &umask), 0o755);
    }
}
