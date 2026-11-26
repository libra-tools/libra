//! Opt-in repair for GitHub SSH connections that are cut before Git negotiation.
//!
//! GitHub serves the same SSH endpoint on `ssh.github.com:443`, which keeps
//! working on networks where port 22 is blocked, reset or throttled. When an
//! interactive command hits such a failure, [`super::ssh_client::SshClient`]
//! asks the user whether to route `github.com` through that endpoint and, on an
//! explicit yes, appends the block GitHub documents to the user's `ssh_config`.
//!
//! This module owns the local, side-effecting half of that flow: the prompt
//! text, the answer parsing and the reversible `ssh_config` edit. It never
//! talks to the network and never replays remote output.

use std::{
    io,
    path::{Path, PathBuf},
};

/// The host users write in GitHub SSH URLs.
pub(crate) const GITHUB_SSH_HOST: &str = "github.com";
/// GitHub's SSH endpoint that also listens on the HTTPS port.
pub(crate) const GITHUB_SSH_ALT_HOST: &str = "ssh.github.com";
/// The HTTPS port `GITHUB_SSH_ALT_HOST` accepts SSH on.
pub(crate) const GITHUB_SSH_ALT_PORT: u16 = 443;

/// Maximum number of prompts before an unrecognized answer is treated as "no".
pub(crate) const MAX_PROMPT_ATTEMPTS: u32 = 3;

/// The `ssh_config` block GitHub documents for SSH over the HTTPS port.
const GITHUB_SSH_OVER_443_BLOCK: &str = "\
# Added by Libra: reach GitHub over SSH on port 443 (ssh.github.com).
Host github.com
    Hostname ssh.github.com
    Port 443
    User git
";

/// The user's `ssh_config` path (`~/.ssh/config`), resolved the same way the
/// known-hosts fallback resolves the home directory. Returns `None` when no
/// home directory is known, so the caller skips the offer instead of writing
/// to a relative path.
pub(crate) fn user_ssh_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|value| !value.is_empty())?;
    Some(PathBuf::from(home).join(".ssh").join("config"))
}

/// Build the confirmation prompt. `failure` is Libra's own classification
/// message, never remote stderr.
pub(crate) fn prompt(config_path: &Path, failure: &str) -> String {
    let mut text = format!(
        "{failure}.\nGitHub also accepts SSH on port {GITHUB_SSH_ALT_PORT} through {GITHUB_SSH_ALT_HOST}, which usually works when port 22 is blocked or unstable.\nLibra can add this block to {}:\n\n",
        config_path.display()
    );
    for line in GITHUB_SSH_OVER_443_BLOCK.lines() {
        text.push_str("    ");
        text.push_str(line);
        text.push('\n');
    }
    text.push_str("\nAdd it and retry (yes/no)? ");
    text
}

/// Resolve one prompt answer: `Some(true)` for an explicit yes, `Some(false)`
/// for no or an empty/EOF answer, `None` for anything else (re-prompt).
pub(crate) fn resolve_answer(answer: &str) -> Option<bool> {
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        return Some(false);
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => None,
    }
}

/// Receipt for an appended block, kept until `ssh -G` confirms the new target
/// so an edit that another `ssh_config` entry overrides can be undone.
#[derive(Debug)]
pub(crate) struct AppendedBlock {
    path: PathBuf,
    /// Length of the file before the append; `None` when Libra created it.
    original_len: Option<u64>,
}

impl AppendedBlock {
    /// Undo the append: truncate back to the previous length, or remove the
    /// file when Libra created it.
    pub(crate) fn roll_back(self) -> io::Result<()> {
        match self.original_len {
            Some(len) => std::fs::OpenOptions::new()
                .write(true)
                .open(&self.path)?
                .set_len(len),
            None => std::fs::remove_file(&self.path),
        }
    }
}

/// Append the SSH-over-443 block to `path`.
///
/// The file is edited in place rather than replaced, so a symlinked
/// `ssh_config` (a common dotfiles setup) keeps its target, owner and mode. A
/// missing file is created with mode `0600` inside a `0700` directory, which
/// is what OpenSSH expects for user configuration.
pub(crate) fn append_block(path: &Path) -> io::Result<AppendedBlock> {
    use std::io::Write;

    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        create_private_dir(parent)?;
    }

    let existing = match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let original_len = existing.as_ref().map(|bytes| bytes.len() as u64);

    // Keep the block on its own lines and visually separate from what the
    // user already has: a missing trailing newline would otherwise glue the
    // comment onto the last option.
    let mut addition = String::new();
    if let Some(bytes) = existing.as_deref()
        && !bytes.is_empty()
    {
        if !bytes.ends_with(b"\n") {
            addition.push('\n');
        }
        addition.push('\n');
    }
    addition.push_str(GITHUB_SSH_OVER_443_BLOCK);

    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let receipt = AppendedBlock {
        path: path.to_path_buf(),
        original_len,
    };
    if let Err(error) = file
        .write_all(addition.as_bytes())
        .and_then(|()| file.flush())
    {
        drop(file);
        // A partial write must not leave half a block behind.
        let _ = receipt.roll_back();
        return Err(error);
    }
    Ok(receipt)
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_resolve_yes_no_and_reprompt() {
        for yes in ["y", "Y", "yes", " YES \n"] {
            assert_eq!(resolve_answer(yes), Some(true), "{yes:?}");
        }
        for no in ["n", "No", "no\n", "", "  \n"] {
            assert_eq!(resolve_answer(no), Some(false), "{no:?}");
        }
        for other in ["maybe", "ye", "443"] {
            assert_eq!(resolve_answer(other), None, "{other:?}");
        }
    }

    #[test]
    fn prompt_shows_destination_and_exact_block() {
        let text = prompt(
            Path::new("/home/u/.ssh/config"),
            "SSH connection failed: connection timed out",
        );
        assert!(text.starts_with("SSH connection failed: connection timed out.\n"));
        assert!(text.contains("/home/u/.ssh/config"));
        assert!(text.contains("    Host github.com\n"));
        assert!(text.contains("        Hostname ssh.github.com\n"));
        assert!(text.contains("        Port 443\n"));
        assert!(text.ends_with("(yes/no)? "));
    }

    #[test]
    fn append_creates_missing_config_and_rolls_back_by_removing_it() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(".ssh").join("config");

        let receipt = append_block(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            GITHUB_SSH_OVER_443_BLOCK
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(path.parent().unwrap()), 0o700);
        }

        receipt.roll_back().unwrap();
        assert!(!path.exists(), "a created config is removed on rollback");
    }

    #[test]
    fn append_keeps_existing_content_and_rolls_back_to_it() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config");
        // No trailing newline: the block must still start on its own line.
        let original = "Host *\n    ServerAliveInterval 60";
        std::fs::write(&path, original).unwrap();

        let receipt = append_block(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{original}\n\n{GITHUB_SSH_OVER_443_BLOCK}")
        );

        receipt.roll_back().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[cfg(unix)]
    #[test]
    fn append_edits_a_symlinked_config_in_place() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("dotfiles_ssh_config");
        let link = root.path().join("config");
        std::fs::write(&target, "Host example\n    User me\n").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        append_block(&link).unwrap();

        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink must survive the edit"
        );
        assert!(
            std::fs::read_to_string(&target)
                .unwrap()
                .ends_with(GITHUB_SSH_OVER_443_BLOCK)
        );
    }
}
