//! SSH protocol client that spawns an `ssh` subprocess for Git transport.
//!
//! Supports both `ssh://[user@]host[:port]/path` and `user@host:path` URL formats.
//! Uses the vault-generated SSH private key for authentication when available.

use std::{
    io::{Error as IoError, ErrorKind, IsTerminal},
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use futures_util::stream::StreamExt;
use git_internal::errors::GitError;
use tempfile::NamedTempFile;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_stream::wrappers::ReceiverStream;

use super::{
    DiscoveryResult, FetchStream, generate_upload_pack_content, parse_discovered_references,
};
use crate::git_protocol::ServiceType;

const DEFAULT_SSH_PORT: u16 = 22;

/// Default idle timeout for SSH I/O operations. Read/write loops reset this
/// timeout after each successful I/O operation; process-wait phases use it as
/// the maximum silent processing window.
const DEFAULT_SSH_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const SSH_SEND_PACK_CHUNK_SIZE: usize = 64 * 1024;

fn default_ssh_idle_timeout() -> Duration {
    #[cfg(test)]
    if let Ok(raw) = std::env::var("LIBRA_TEST_SSH_IDLE_TIMEOUT_MS")
        && let Ok(ms) = raw.parse::<u64>()
        && ms > 0
    {
        return Duration::from_millis(ms);
    }

    DEFAULT_SSH_IDLE_TIMEOUT
}

pub struct SshClient {
    user: String,
    host: String,
    port: u16,
    repo_path: String,
    key_path: Option<String>,
    temp_key_file: Option<NamedTempFile>,
    strict_host_key_checking: String,
    idle_timeout: Duration,
}

impl SshClient {
    /// Parse an SSH URL in either `ssh://[user@]host[:port]/path` or `user@host:path` format.
    pub fn from_ssh_spec(spec: &str) -> Result<Self, String> {
        if spec.starts_with("ssh://") {
            Self::from_ssh_url(spec)
        } else {
            Self::from_scp_style(spec)
        }
    }

    /// Set the path to the SSH private key for authentication.
    pub fn with_key_path(mut self, key_path: String) -> Self {
        self.key_path = Some(key_path);
        self
    }

    /// Hold a temporary SSH private key file for the lifetime of the client.
    pub fn with_temp_key_file(mut self, temp_key_file: NamedTempFile) -> Self {
        self.temp_key_file = Some(temp_key_file);
        self.key_path = None;
        self
    }

    /// Override the SSH per-operation idle timeout for callers that need a
    /// longer-lived transport, such as push send-pack.
    pub fn with_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.idle_timeout = idle_timeout;
        self
    }

    /// Configure StrictHostKeyChecking mode.
    ///
    /// Supported values: `ask` (default), `yes`, `accept-new`, `no` — the same
    /// four policies OpenSSH/Git expose. In `ask` mode the option is not passed
    /// to `ssh` at all, so the user's `~/.ssh/config` governs, matching Git.
    pub fn with_strict_host_key_checking(mut self, mode: String) -> Result<Self, String> {
        let normalized = normalize_host_key_checking_mode(&mode).ok_or_else(|| {
            format!(
                "invalid ssh.strictHostKeyChecking value '{mode}', \
                 expected 'ask', 'yes', 'accept-new', or 'no'"
            )
        })?;
        self.strict_host_key_checking = normalized.to_string();
        Ok(self)
    }

    fn from_ssh_url(spec: &str) -> Result<Self, String> {
        let url = url::Url::parse(spec).map_err(|e| format!("invalid SSH URL: {e}"))?;
        let user = if url.username().is_empty() {
            "git".to_string()
        } else {
            url.username().to_string()
        };
        let host = url.host_str().ok_or("missing host in SSH URL")?.to_string();
        let port = url.port().unwrap_or(DEFAULT_SSH_PORT);
        let mut repo_path = url.path().to_string();
        if repo_path.starts_with('/') {
            repo_path = repo_path[1..].to_string();
        }
        if repo_path.ends_with('/') && repo_path.len() > 1 {
            repo_path.pop();
        }
        Ok(Self {
            user,
            host,
            port,
            repo_path,
            key_path: None,
            temp_key_file: None,
            strict_host_key_checking: "ask".to_string(),
            idle_timeout: default_ssh_idle_timeout(),
        })
    }

    /// Parse SCP-style `user@host:path` format.
    fn from_scp_style(spec: &str) -> Result<Self, String> {
        let (user_host, path) = spec
            .split_once(':')
            .ok_or_else(|| format!("invalid SCP-style SSH spec: {spec}"))?;
        let (user, host) = if let Some((u, h)) = user_host.split_once('@') {
            (u.to_string(), h.to_string())
        } else {
            ("git".to_string(), user_host.to_string())
        };
        let repo_path = path.trim_end_matches('/').to_string();
        Ok(Self {
            user,
            host,
            port: DEFAULT_SSH_PORT,
            repo_path,
            key_path: None,
            temp_key_file: None,
            strict_host_key_checking: "ask".to_string(),
            idle_timeout: default_ssh_idle_timeout(),
        })
    }

    /// Spawn an SSH subprocess running the given Git service on the remote.
    ///
    /// Host key checking mirrors Git's transport: in the default `ask` mode no
    /// `StrictHostKeyChecking` option is passed, so the user's `~/.ssh/config`
    /// governs and OpenSSH offers its interactive trust prompt (TOFU) on the
    /// terminal. Explicit modes are forwarded verbatim.
    ///
    /// Interactivity is decided by libra's own stdin: in headless contexts
    /// (CI, agents, tests) prompts can never be answered, so `BatchMode=yes`
    /// makes ssh fail fast instead of hanging, and stderr stays piped so
    /// diagnostics land in the error message. Interactive sessions inherit
    /// stderr so the user sees ssh's host-key warning, fingerprint, and
    /// "Permanently added" confirmation live — exactly like `git clone`.
    async fn spawn_service(&self, service: ServiceType) -> Result<tokio::process::Child, IoError> {
        let service_cmd = match service {
            ServiceType::UploadPack => "git-upload-pack",
            ServiceType::ReceivePack => "git-receive-pack",
        };
        // Build: ssh [opts] user@host "git-upload-pack '/repo/path'"
        let ssh_bin = std::env::var("LIBRA_SSH_COMMAND").unwrap_or_else(|_| "ssh".to_string());
        let interactive = std::io::stdin().is_terminal();
        let mut cmd = tokio::process::Command::new(ssh_bin);
        // In `ask` mode (default) defer to the user's ssh_config, like Git.
        if self.strict_host_key_checking != "ask" {
            cmd.arg("-o").arg(format!(
                "StrictHostKeyChecking={}",
                self.strict_host_key_checking
            ));
        }
        if !interactive {
            cmd.arg("-o").arg("BatchMode=yes");
        }
        if let Some(ref key_file) = self.temp_key_file {
            cmd.arg("-i").arg(key_file.path());
        } else if let Some(ref key) = self.key_path {
            cmd.arg("-i").arg(key);
        }
        if self.port != DEFAULT_SSH_PORT {
            cmd.arg("-p").arg(self.port.to_string());
        }
        cmd.arg(format!("{}@{}", self.user, self.host));
        cmd.arg(format!(
            "{service_cmd} {}",
            shell_single_quote(&self.repo_path)
        ));
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        if interactive {
            // Let ssh talk to the user's terminal directly (host-key prompt
            // context, banners, remote diagnostics), as Git does.
            cmd.stderr(std::process::Stdio::inherit());
        } else {
            cmd.stderr(std::process::Stdio::piped());
        }
        // The local `ssh` process can outlive the remote service (GitHub in
        // particular keeps the channel open briefly, and ControlMaster
        // setups can keep the client process alive even longer). Killing
        // on drop ensures `fetch_objects`'s background task cannot leave
        // an orphaned subprocess blocking shutdown.
        cmd.kill_on_drop(true).spawn()
    }

    /// Read pkt-line advertisement from the SSH child's stdout.
    ///
    /// Each individual `read_exact` call is wrapped with the configured idle
    /// timeout so a stalled remote triggers a timely error instead of blocking
    /// forever.
    async fn read_advertisement(
        &self,
        stdout: &mut tokio::process::ChildStdout,
    ) -> Result<Bytes, IoError> {
        let mut buf = BytesMut::new();
        loop {
            let mut len_buf = [0u8; 4];
            let timeout = self.idle_timeout;
            tokio::time::timeout(timeout, stdout.read_exact(&mut len_buf))
                .await
                .map_err(|_| {
                    IoError::other(format!(
                        "SSH read timed out after {}s (idle)",
                        timeout.as_secs()
                    ))
                })?
                .map_err(|e| IoError::other(format!("SSH read failed: {e}")))?;
            let len_str = std::str::from_utf8(&len_buf)
                .map_err(|e| IoError::other(format!("invalid pkt-line length: {e}")))?;
            let len = usize::from_str_radix(len_str, 16)
                .map_err(|e| IoError::other(format!("invalid pkt-line length: {e}")))?;
            buf.extend_from_slice(&len_buf);
            if len == 0 {
                break;
            }
            let mut data = vec![0u8; len - 4];
            let timeout = self.idle_timeout;
            tokio::time::timeout(timeout, stdout.read_exact(&mut data))
                .await
                .map_err(|_| {
                    IoError::other(format!(
                        "SSH read timed out after {}s (idle)",
                        timeout.as_secs()
                    ))
                })?
                .map_err(|e| IoError::other(format!("SSH read failed: {e}")))?;
            buf.extend_from_slice(&data);
        }
        Ok(buf.freeze())
    }

    pub async fn discovery_reference(
        &self,
        service: ServiceType,
    ) -> Result<DiscoveryResult, GitError> {
        let mut child = self
            .spawn_service(service)
            .await
            .map_err(|e| GitError::NetworkError(format!("SSH spawn failed: {e}")))?;
        let response = {
            let stdout = child.stdout.as_mut().ok_or_else(|| {
                GitError::NetworkError("SSH child stdout not captured".to_string())
            })?;
            self.read_advertisement(stdout).await
        };
        let response = match response {
            Ok(response) => response,
            Err(read_err) => {
                let output = child.wait_with_output().await.map_err(|wait_err| {
                    GitError::NetworkError(format!(
                        "SSH read failed: {read_err}; unable to collect process output: {wait_err}"
                    ))
                })?;
                return Err(GitError::NetworkError(format!(
                    "SSH read failed: {read_err}; {}",
                    describe_process_output(&output)
                )));
            }
        };
        // Discovery only needs the advertisement packet. Kill and reap the child
        // to avoid leaving an unreaped process around.
        let _ = child.kill().await;
        let output = child
            .wait_with_output()
            .await
            .map_err(|e| GitError::NetworkError(format!("SSH wait failed: {e}")))?;
        // If the process was not killed by signal and exited non-zero, surface diagnostics.
        if !output.status.success() && output.status.code().is_some() {
            return Err(GitError::NetworkError(format!(
                "SSH discovery command failed: {}",
                describe_process_output(&output)
            )));
        }
        parse_discovered_references(response, service)
    }

    pub async fn fetch_objects(
        &self,
        have: &[String],
        want: &[String],
        shallow: &[String],
        depth: Option<usize>,
    ) -> Result<FetchStream, IoError> {
        let mut child = self.spawn_service(ServiceType::UploadPack).await?;
        let advertisement = {
            let stdout = child
                .stdout
                .as_mut()
                .ok_or_else(|| IoError::other("SSH child stdout not captured"))?;
            self.read_advertisement(stdout).await
        };
        if let Err(read_err) = advertisement {
            let output = child.wait_with_output().await.map_err(|wait_err| {
                IoError::other(format!(
                    "SSH advertisement read failed: {read_err}; unable to collect process output: {wait_err}"
                ))
            })?;
            return Err(IoError::other(format!(
                "SSH advertisement read failed: {read_err}; {}",
                describe_process_output(&output)
            )));
        }

        // Send the upload-pack request
        let body = generate_upload_pack_content(have, want, shallow, depth);
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| IoError::other("SSH child stdin not captured"))?;
        stdin.write_all(&body).await?;
        stdin.shutdown().await?;

        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| IoError::other("SSH child stdout not captured"))?;
        // stderr may be uncaptured when it is inherited in interactive
        // sessions; treat that the same as an empty stream.
        let stderr = child.stderr.take();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, IoError>>(32);
        let idle_timeout = self.idle_timeout;

        tokio::spawn(async move {
            let stderr_task = tokio::spawn(async move {
                let mut buf = Vec::new();
                if let Some(mut stderr) = stderr {
                    let _ = stderr.read_to_end(&mut buf).await;
                }
                buf
            });

            let mut buf = [0u8; 16 * 1024];
            let mut forward_err: Option<IoError> = None;
            let mut sent_any_stdout = false;
            loop {
                match tokio::time::timeout(idle_timeout, stdout.read(&mut buf)).await {
                    Err(_) => {
                        let _ = child.start_kill();
                        if !sent_any_stdout {
                            forward_err = Some(IoError::new(
                                std::io::ErrorKind::TimedOut,
                                format!(
                                    "SSH upload-pack stdout timed out after {}s (idle)",
                                    idle_timeout.as_secs()
                                ),
                            ));
                        }
                        break;
                    }
                    Ok(Ok(0)) => break,
                    Ok(Ok(n)) => {
                        sent_any_stdout = true;
                        if tx
                            .send(Ok(Bytes::copy_from_slice(&buf[..n])))
                            .await
                            .is_err()
                        {
                            // Consumer dropped the stream; rely on
                            // `kill_on_drop` (set in `spawn_service`) to take
                            // down the ssh subprocess when `child` is dropped.
                            stderr_task.abort();
                            return;
                        }
                    }
                    Ok(Err(err)) => {
                        forward_err = Some(IoError::other(format!(
                            "failed to read SSH upload-pack stdout: {err}"
                        )));
                        break;
                    }
                }
            }

            // After stdout EOF the upload-pack service is finished. The local
            // `ssh` process can still take a while to exit (control sockets,
            // late exit-status, server-side keepalive), so don't block the
            // consumer's stream on a clean exit — wait briefly, then kill.
            let status = match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                Ok(Ok(status)) => Some(status),
                Ok(Err(_)) => None,
                Err(_) => {
                    let _ = child.start_kill();
                    let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
                    None
                }
            };

            // Stderr stays open until the ssh process actually exits; if we
            // had to kill it above the read may already be unblocked, but cap
            // the join anyway so a stuck pipe can't keep the channel alive.
            let stderr_buf = match tokio::time::timeout(Duration::from_secs(1), stderr_task).await {
                Ok(Ok(buf)) => buf,
                _ => Vec::new(),
            };

            if let Some(err) = forward_err {
                let _ = tx.send(Err(err)).await;
            } else if let Some(status) = status
                && !status.success()
            {
                let _ = tx
                    .send(Err(IoError::other(format!(
                        "SSH upload-pack failed: {}",
                        describe_status_with_stderr(&status, &stderr_buf)
                    ))))
                    .await;
            }
        });

        Ok(ReceiverStream::new(rx).boxed())
    }

    pub async fn send_pack(&self, data: Bytes) -> Result<Bytes, IoError> {
        let mut child = self.spawn_service(ServiceType::ReceivePack).await?;
        let advertisement = {
            let stdout = child
                .stdout
                .as_mut()
                .ok_or_else(|| IoError::other("SSH child stdout not captured"))?;
            self.read_advertisement(stdout).await
        };
        if let Err(read_err) = advertisement {
            let output = child.wait_with_output().await.map_err(|wait_err| {
                IoError::other(format!(
                    "SSH advertisement read failed: {read_err}; unable to collect process output: {wait_err}"
                ))
            })?;
            return Err(IoError::other(format!(
                "SSH advertisement read failed: {read_err}; {}",
                describe_process_output(&output)
            )));
        }

        // Send the pack data with the timeout resetting after each successful
        // write. A single write_all over the whole pack would incorrectly turn
        // this into a total-transfer timeout for large pushes.
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| IoError::other("SSH child stdin not captured"))?;
        Self::write_all_with_idle_timeout(stdin, data.as_ref(), self.idle_timeout).await?;
        let timeout = self.idle_timeout;
        tokio::time::timeout(timeout, stdin.shutdown())
            .await
            .map_err(|_| {
                IoError::other(format!(
                    "SSH shutdown timed out after {}s (idle)",
                    timeout.as_secs()
                ))
            })?
            .map_err(|e| IoError::other(format!("SSH shutdown failed: {e}")))?;

        // Wait for remote to process the pack (with idle timeout)
        let timeout = self.idle_timeout;
        let output = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| {
                IoError::other(format!(
                    "SSH receive-pack timed out after {}s (idle)",
                    timeout.as_secs()
                ))
            })?
            .map_err(|e| IoError::other(format!("SSH wait failed: {e}")))?;
        if !output.status.success() {
            return Err(IoError::other(format!(
                "SSH receive-pack failed: {}",
                describe_process_output(&output)
            )));
        }
        Ok(Bytes::from(output.stdout))
    }

    async fn write_all_with_idle_timeout<W>(
        writer: &mut W,
        data: &[u8],
        idle_timeout: Duration,
    ) -> Result<(), IoError>
    where
        W: AsyncWrite + Unpin,
    {
        let mut written = 0;
        while written < data.len() {
            let chunk_end = (written + SSH_SEND_PACK_CHUNK_SIZE).min(data.len());
            let n = tokio::time::timeout(idle_timeout, writer.write(&data[written..chunk_end]))
                .await
                .map_err(|_| {
                    IoError::new(
                        ErrorKind::TimedOut,
                        format!(
                            "SSH write timed out after {}s (idle)",
                            idle_timeout.as_secs()
                        ),
                    )
                })?
                .map_err(|e| IoError::other(format!("SSH write failed: {e}")))?;
            if n == 0 {
                return Err(IoError::new(
                    ErrorKind::WriteZero,
                    "SSH write failed: wrote zero bytes",
                ));
            }
            written += n;
        }
        Ok(())
    }
}

fn describe_process_output(output: &std::process::Output) -> String {
    describe_status_with_stderr(&output.status, &output.stderr)
}

fn describe_status_with_stderr(status: &std::process::ExitStatus, stderr: &[u8]) -> String {
    let status = status.code().map_or_else(
        || "terminated by signal".to_string(),
        |code| code.to_string(),
    );
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    if stderr.is_empty() {
        format!("exit status {status}")
    } else {
        format!("exit status {status}, stderr: {stderr}")
    }
}

fn normalize_host_key_checking_mode(mode: &str) -> Option<&'static str> {
    ["ask", "yes", "accept-new", "no"]
        .into_iter()
        .find(|known| mode.eq_ignore_ascii_case(known))
}

fn shell_single_quote(value: &str) -> String {
    let escaped = value.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

/// Check if a remote spec looks like an SSH URL.
pub fn is_ssh_spec(spec: &str) -> bool {
    if spec.starts_with("ssh://") {
        return true;
    }

    // SCP-style: [user@]host:path
    if spec.contains("://")
        || spec.starts_with('/')
        || spec.starts_with("./")
        || spec.starts_with("../")
    {
        return false;
    }

    let Some((user_host, path)) = spec.split_once(':') else {
        return false;
    };
    if user_host.is_empty() || path.is_empty() {
        return false;
    }

    // Avoid mistaking Windows local paths (e.g. C:\repo) for SSH remotes.
    if user_host.len() == 1
        && user_host
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic())
    {
        return false;
    }

    if user_host.contains('/') || user_host.contains('\\') {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_ssh_spec() {
        assert!(is_ssh_spec("git@github.com:user/repo.git"));
        assert!(is_ssh_spec("github.com:user/repo.git"));
        assert!(is_ssh_spec("ssh://git@github.com/user/repo.git"));
        assert!(is_ssh_spec("ssh://github.com/user/repo.git"));
        assert!(!is_ssh_spec("https://github.com/user/repo.git"));
        assert!(!is_ssh_spec("git://github.com/user/repo.git"));
        assert!(!is_ssh_spec("/local/path/to/repo"));
        assert!(!is_ssh_spec("C:\\repo\\path"));
        assert!(!is_ssh_spec("foo/bar:baz"));
    }

    #[test]
    fn test_parse_scp_style() {
        let client = SshClient::from_scp_style("git@github.com:user/repo.git").unwrap();
        assert_eq!(client.user, "git");
        assert_eq!(client.host, "github.com");
        assert_eq!(client.repo_path, "user/repo.git");
        assert_eq!(client.port, 22);
    }

    #[test]
    fn test_parse_ssh_url() {
        let client = SshClient::from_ssh_url("ssh://git@github.com:2222/user/repo.git").unwrap();
        assert_eq!(client.user, "git");
        assert_eq!(client.host, "github.com");
        assert_eq!(client.repo_path, "user/repo.git");
        assert_eq!(client.port, 2222);
    }

    #[test]
    fn test_parse_ssh_url_default_user() {
        let client = SshClient::from_ssh_url("ssh://github.com/user/repo.git").unwrap();
        assert_eq!(client.user, "git");
        assert_eq!(client.host, "github.com");
    }

    #[test]
    fn test_shell_single_quote() {
        assert_eq!(shell_single_quote("user/repo.git"), "'user/repo.git'");
        assert_eq!(
            shell_single_quote("user/repo'weird.git"),
            "'user/repo'\"'\"'weird.git'"
        );
    }

    #[test]
    fn test_default_host_key_checking_is_ask() {
        // Git parity: the default defers to the user's ssh_config and lets
        // OpenSSH run its interactive TOFU prompt; no option is injected.
        let client = SshClient::from_scp_style("git@github.com:user/repo.git").unwrap();
        assert_eq!(client.strict_host_key_checking, "ask");
        let client = SshClient::from_ssh_url("ssh://git@github.com/user/repo.git").unwrap();
        assert_eq!(client.strict_host_key_checking, "ask");
    }

    #[test]
    fn test_with_strict_host_key_checking_accepts_git_modes() {
        for mode in ["ask", "yes", "accept-new", "no", "ACCEPT-NEW"] {
            let client = SshClient::from_scp_style("git@github.com:user/repo.git")
                .unwrap()
                .with_strict_host_key_checking(mode.to_string())
                .unwrap();
            assert_eq!(client.strict_host_key_checking, mode.to_lowercase());
        }
    }

    #[test]
    fn test_with_strict_host_key_checking_invalid_value() {
        let result = SshClient::from_scp_style("git@github.com:user/repo.git")
            .unwrap()
            .with_strict_host_key_checking("bogus".to_string());
        assert!(result.is_err(), "invalid mode should be rejected");
        let err = result.err().unwrap();
        assert!(err.contains("expected 'ask', 'yes', 'accept-new', or 'no'"));
    }

    #[tokio::test]
    async fn send_pack_write_helper_writes_large_payload_in_chunks() {
        let (mut writer, mut reader) = tokio::io::duplex(1024);
        let data = vec![42u8; SSH_SEND_PACK_CHUNK_SIZE * 2 + 17];
        let expected_len = data.len();

        let reader_task = tokio::spawn(async move {
            let mut received = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut received)
                .await
                .expect("duplex reader should drain written bytes");
            received.len()
        });

        SshClient::write_all_with_idle_timeout(&mut writer, &data, Duration::from_secs(1))
            .await
            .expect("chunked write should complete while the reader drains data");
        writer
            .shutdown()
            .await
            .expect("duplex writer should shut down cleanly");

        assert_eq!(
            reader_task.await.expect("reader task should finish"),
            expected_len
        );
    }
}
