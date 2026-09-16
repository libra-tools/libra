//! SSH protocol client that spawns an `ssh` subprocess for Git transport.
//!
//! Supports both `ssh://[user@]host[:port]/path` and `user@host:path` URL formats.
//! Uses the vault-generated SSH private key for authentication when available.

use std::{
    io::{Error as IoError, ErrorKind},
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use futures_util::stream::StreamExt;
use git_internal::errors::GitError;
use sha2::Digest;
use tempfile::NamedTempFile;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_stream::wrappers::ReceiverStream;
use tracing::instrument::WithSubscriber;

use super::{
    DiscoveryResult, FetchStream, generate_upload_pack_content, parse_discovered_references,
};
use crate::{
    command::fetch::is_pkt_line_io_error,
    git_protocol::{PktLineError, ServiceType, pkt_frame_payload_len, pkt_line_read_error},
};

const DEFAULT_SSH_PORT: u16 = 22;

const SSH_STDERR_LIMIT: usize = 64 * 1024;
const SSH_PROTOCOL_OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
pub(crate) const SSH_HOST_KEY_UNCONFIRMED_SIGNAL: &str = "SSH host trust needs confirmation: ";
pub(crate) const SSH_HOST_KEY_GUIDANCE: &str = "verify the host fingerprint through a trusted provider console or another trusted channel before manually updating ~/.ssh/known_hosts; alternatively make a separate interactive SSH connection using the repository SSH user, host and port, and compare the displayed fingerprint before accepting it; review ssh.strictHostKeyChecking";
pub(crate) const SSH_HOST_KEY_CHANGED_SIGNAL: &str = "SSH host identity changed: ";
pub(crate) const SSH_HOST_KEY_CHANGED_GUIDANCE: &str = "the SSH host identity has changed, which may indicate interception or a legitimate key rotation; verify the new fingerprint through a trusted channel before replacing any existing entry in ~/.ssh/known_hosts; do not bypass host-key checking";

struct SshCapturedBytes {
    bytes: Vec<u8>,
    total: u64,
    digest: [u8; 32],
}

impl SshCapturedBytes {
    // Inert placeholder for stdout already consumed by the protocol reader.
    // Its metadata is never used for diagnostics; only stderr metadata is logged.
    fn empty() -> Self {
        Self {
            bytes: Vec::new(),
            total: 0,
            digest: sha2::Sha256::digest([]).into(),
        }
    }

    #[cfg(test)]
    fn from_fixture(bytes: &[u8], limit: usize) -> Self {
        Self {
            bytes: bytes[..bytes.len().min(limit)].to_vec(),
            total: bytes.len() as u64,
            digest: sha2::Sha256::digest(bytes).into(),
        }
    }
}

// Deliberately omit retained bytes, including from Debug and task error paths.
impl std::fmt::Debug for SshCapturedBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshCapturedBytes")
            .field("total", &self.total)
            .field("retained", &self.bytes.len())
            .field("sha256", &hex::encode(self.digest))
            .finish()
    }
}

struct SshCaptureTask {
    task: tokio::task::JoinHandle<Result<SshCapturedBytes, IoError>>,
}

impl SshCaptureTask {
    fn start<R>(mut reader: R, limit: usize) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let task = tokio::spawn(async move {
            let mut bytes = Vec::new();
            let mut total = 0u64;
            let mut digest = sha2::Sha256::new();
            let mut chunk = [0u8; 16 * 1024];
            loop {
                let count = reader.read(&mut chunk).await.map_err(|error| {
                    IoError::new(error.kind(), "unable to read captured SSH output")
                })?;
                if count == 0 {
                    break;
                }
                total = total.saturating_add(count as u64);
                digest.update(&chunk[..count]);
                let keep = count.min(limit.saturating_sub(bytes.len()));
                bytes.extend_from_slice(&chunk[..keep]);
            }
            Ok(SshCapturedBytes {
                bytes,
                total,
                digest: digest.finalize().into(),
            })
        });
        Self { task }
    }

    async fn finish(mut self, deadline: tokio::time::Instant) -> Result<SshCapturedBytes, IoError> {
        tokio::time::timeout_at(deadline, &mut self.task)
            .await
            .map_err(|_| {
                IoError::new(
                    ErrorKind::TimedOut,
                    "captured SSH output did not close before the cleanup deadline",
                )
            })?
            .map_err(|_| IoError::other("SSH output collection task failed"))?
    }
}

impl Drop for SshCaptureTask {
    fn drop(&mut self) {
        // Dropping an owned JoinHandle alone detaches it. Abort explicitly so
        // cancellation or a descendant-held pipe cannot leak a collector task.
        self.task.abort();
    }
}

#[derive(Debug)]
struct SshProcessOutput {
    stdout_observed: bool,
    status: std::process::ExitStatus,
    stdout: SshCapturedBytes,
    stderr: Option<SshCapturedBytes>,
}

struct SshProcess {
    stdout_observed: bool,
    child: tokio::process::Child,
    stderr_capture: SshCaptureTask,
}

impl std::ops::Deref for SshProcess {
    type Target = tokio::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.child
    }
}
impl std::ops::DerefMut for SshProcess {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.child
    }
}

impl SshProcess {
    fn new(mut child: tokio::process::Child) -> Result<Self, IoError> {
        let stderr = child.stderr.take().ok_or_else(|| {
            IoError::other("SSH child stderr was not captured; restart the operation")
        })?;
        // Drain immediately, before advertisement reads and potentially blocked
        // pack writes. The retained prefix is bounded; the digest covers all bytes.
        let stderr_capture = SshCaptureTask::start(stderr, SSH_STDERR_LIMIT);
        Ok(Self {
            child,
            stderr_capture,
            stdout_observed: false,
        })
    }

    async fn collect_output(
        mut self,
        deadline: tokio::time::Instant,
        stdout_limit: usize,
    ) -> Result<SshProcessOutput, IoError> {
        drop(self.child.stdin.take());
        let stdout = self
            .child
            .stdout
            .take()
            .map(|pipe| SshCaptureTask::start(pipe, stdout_limit));
        let status = tokio::time::timeout_at(deadline, self.child.wait())
            .await
            .map_err(|_| {
                IoError::new(
                    ErrorKind::TimedOut,
                    "SSH process did not exit before the cleanup deadline",
                )
            })?
            .map_err(|_| IoError::other("unable to collect SSH process exit status"))?;
        let stderr = finish_stderr_capture(self.stderr_capture, deadline).await;
        let stdout = match stdout {
            Some(task) => task.finish(deadline).await?,
            None => SshCapturedBytes::empty(),
        };
        Ok(SshProcessOutput {
            stdout_observed: self.stdout_observed,
            status,
            stdout,
            stderr,
        })
    }
}

// Stderr is diagnostic metadata. A completed payload and observed exit status
// remain usable if a descendant-held stderr pipe outlives bounded collection.
// None deliberately carries no fabricated empty-stream digest or byte count.
async fn finish_stderr_capture(
    capture: SshCaptureTask,
    deadline: tokio::time::Instant,
) -> Option<SshCapturedBytes> {
    match capture.finish(deadline).await {
        Ok(bytes) => Some(bytes),
        Err(_) => {
            tracing::debug!("SSH stderr diagnostics unavailable after bounded collection");
            None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SshHostKeyUnconfirmed {
    Untrusted,
    Changed,
}
impl SshHostKeyUnconfirmed {
    fn message(self) -> &'static str {
        match self {
            Self::Untrusted => "SSH host key could not be verified",
            Self::Changed => "SSH host identity has changed",
        }
    }
    fn guidance(self) -> &'static str {
        match self {
            Self::Untrusted => SSH_HOST_KEY_GUIDANCE,
            Self::Changed => SSH_HOST_KEY_CHANGED_GUIDANCE,
        }
    }
    fn signal(self) -> &'static str {
        match self {
            Self::Untrusted => SSH_HOST_KEY_UNCONFIRMED_SIGNAL,
            Self::Changed => SSH_HOST_KEY_CHANGED_SIGNAL,
        }
    }
}
impl std::fmt::Display for SshHostKeyUnconfirmed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}; {}", self.message(), self.guidance())
    }
}
impl std::error::Error for SshHostKeyUnconfirmed {}

fn ssh_host_key_unconfirmed(
    status: &std::process::ExitStatus,
    stderr: Option<&SshCapturedBytes>,
) -> Option<SshHostKeyUnconfirmed> {
    if status.code() != Some(255) {
        return None;
    }
    let stderr = stderr?;
    // Prefer the changed-key warning when OpenSSH emits both diagnostics.
    for (pattern, kind) in [
        (
            b"remote host identification has changed".as_slice(),
            SshHostKeyUnconfirmed::Changed,
        ),
        (
            b"host key verification failed".as_slice(),
            SshHostKeyUnconfirmed::Untrusted,
        ),
    ] {
        if stderr
            .bytes
            .windows(pattern.len())
            .any(|part| part.eq_ignore_ascii_case(pattern))
        {
            return Some(kind);
        }
    }
    None
}

fn ssh_discovery_read_error(error: IoError) -> GitError {
    if let Some(kind) = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<SshHostKeyUnconfirmed>())
    {
        GitError::NetworkError(format!("{}{kind}", kind.signal()))
    } else {
        GitError::NetworkError(error.to_string())
    }
}

/// Default idle timeout for SSH I/O operations. Read/write loops reset this
/// timeout after each successful I/O operation; process-wait phases use it as
/// the maximum silent processing window.
const DEFAULT_SSH_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const SSH_SEND_PACK_CHUNK_SIZE: usize = 64 * 1024;
/// Maximum wait for the direct SSH child after an advertisement read fails.
const SSH_READ_ERROR_REAP_TIMEOUT: Duration = Duration::from_secs(2);
/// Give an SSH process which closed stdout a short chance to report its exit
/// status. This window is included in the total read-error cleanup budget.
const SSH_HEADER_EOF_STATUS_TIMEOUT: Duration = Duration::from_millis(100);

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
    /// to `ssh` at all, so the user's `~/.ssh/config` governs the policy.
    /// BatchMode still prevents interactive trust and passphrase prompts.
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

    /// Spawn SSH with BatchMode enabled and captured stderr in every context.
    /// Default host-key policy still follows ssh_config, but new trust decisions
    /// and passphrase prompts must be handled separately by the user.
    async fn spawn_service(&self, service: ServiceType) -> Result<SshProcess, IoError> {
        let service_cmd = match service {
            ServiceType::UploadPack => "git-upload-pack",
            ServiceType::ReceivePack => "git-receive-pack",
        };
        // Build: ssh [opts] user@host "git-upload-pack '/repo/path'"
        let ssh_bin = std::env::var("LIBRA_SSH_COMMAND").unwrap_or_else(|_| "ssh".to_string());
        let mut cmd = tokio::process::Command::new(ssh_bin);
        cmd.arg("-o").arg("BatchMode=yes");
        // In `ask` mode defer only the host-key policy to ssh_config; BatchMode
        // still disables interactive trust and passphrase prompts.
        if self.strict_host_key_checking != "ask" {
            cmd.arg("-o").arg(format!(
                "StrictHostKeyChecking={}",
                self.strict_host_key_checking
            ));
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
        cmd.stderr(std::process::Stdio::piped());
        // The local `ssh` process can outlive the remote service (GitHub in
        // particular keeps the channel open briefly, and ControlMaster
        // setups can keep the client process alive even longer). Killing
        // on drop ensures `fetch_objects`'s background task cannot leave
        // an orphaned subprocess blocking shutdown.
        SshProcess::new(cmd.kill_on_drop(true).spawn()?)
    }

    /// Read pkt-line advertisement from the SSH child's stdout.
    ///
    /// Each header or payload read has the configured idle timeout. Record even
    /// a partial first header: once stdout arrives, remote stderr must not be
    /// interpreted as a local host-key verification failure.
    async fn read_advertisement<R: AsyncRead + Unpin>(
        &self,
        stdout: &mut R,
        stdout_observed: &mut bool,
    ) -> Result<Bytes, IoError> {
        let mut buf = BytesMut::new();
        loop {
            let mut len_buf = [0u8; 4];
            let timeout = self.idle_timeout;
            tokio::time::timeout(timeout, async {
                let mut received = 0;
                while received < len_buf.len() {
                    let count = stdout.read(&mut len_buf[received..]).await?;
                    if count == 0 {
                        return Err(IoError::from(ErrorKind::UnexpectedEof));
                    }
                    *stdout_observed = true;
                    received += count;
                }
                Ok::<(), IoError>(())
            })
            .await
            .map_err(|_| {
                IoError::other(format!(
                    "SSH read timed out after {}s (idle)",
                    timeout.as_secs()
                ))
            })?
            .map_err(|error| {
                wrap_ssh_read_error(
                    pkt_line_read_error(error, PktLineError::TruncatedHeader),
                    "SSH read failed",
                    None,
                )
            })?;
            let len_str = std::str::from_utf8(&len_buf)
                .map_err(|e| IoError::other(format!("invalid pkt-line length: {e}")))?;
            let len = usize::from_str_radix(len_str, 16)
                .map_err(|e| IoError::other(format!("invalid pkt-line length: {e}")))?;
            if buf.len().saturating_add(len.max(4)) > SSH_PROTOCOL_OUTPUT_LIMIT {
                return Err(IoError::other(
                    "SSH advertisement exceeded the 16 MiB limit; use the repository's HTTPS URL if available, or ask its maintainer to reduce refs",
                ));
            }
            buf.extend_from_slice(&len_buf);
            if len == 0 {
                break;
            }
            let payload_len = pkt_frame_payload_len(len as u32)
                .map_err(|error| IoError::new(ErrorKind::InvalidData, PktLineError::from(error)))?;
            let mut data = vec![0u8; payload_len];
            let timeout = self.idle_timeout;
            tokio::time::timeout(timeout, stdout.read_exact(&mut data))
                .await
                .map_err(|_| {
                    IoError::other(format!(
                        "SSH read timed out after {}s (idle)",
                        timeout.as_secs()
                    ))
                })?
                .map_err(|error| {
                    wrap_ssh_read_error(
                        pkt_line_read_error(error, PktLineError::TruncatedPayload),
                        "SSH read failed",
                        None,
                    )
                })?;
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
            let stdout = child.child.stdout.as_mut().ok_or_else(|| {
                GitError::NetworkError("SSH child stdout not captured".to_string())
            })?;
            self.read_advertisement(stdout, &mut child.stdout_observed)
                .await
        };
        let response = match response {
            Ok(response) => response,
            Err(read_err) => {
                let error = finish_ssh_read_error(child, read_err, "SSH read failed").await;
                return Err(ssh_discovery_read_error(error));
            }
        };
        // Discovery only needs the advertisement packet. Kill and reap the child
        // to avoid leaving an unreaped process around.
        let deadline = tokio::time::Instant::now() + SSH_READ_ERROR_REAP_TIMEOUT;
        let status_deadline =
            (tokio::time::Instant::now() + SSH_HEADER_EOF_STATUS_TIMEOUT).min(deadline);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Err(_) => {
                    return Err(GitError::NetworkError(
                        "unable to read SSH discovery exit status".to_string(),
                    ));
                }
                Ok(None) if tokio::time::Instant::now() >= status_deadline => {
                    child.start_kill().map_err(|_| {
                        GitError::NetworkError(
                            "unable to stop the SSH discovery process".to_string(),
                        )
                    })?;
                    break;
                }
                Ok(None) => tokio::time::sleep(Duration::from_millis(1)).await,
            }
        }
        drop(child.stdout.take());
        let output = child
            .collect_output(deadline, 0)
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
                .child
                .stdout
                .as_mut()
                .ok_or_else(|| IoError::other("SSH child stdout not captured"))?;
            self.read_advertisement(stdout, &mut child.stdout_observed)
                .await
        };
        if let Err(read_err) = advertisement {
            return Err(
                finish_ssh_read_error(child, read_err, "SSH advertisement read failed").await,
            );
        }

        // Send the upload-pack request
        let body = generate_upload_pack_content(have, want, shallow, depth);
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| IoError::other("SSH child stdin not captured"))?;
        Self::write_all_with_idle_timeout(&mut stdin, &body, self.idle_timeout).await?;
        tokio::time::timeout(self.idle_timeout, stdin.shutdown())
            .await
            .map_err(|_| {
                IoError::new(
                    ErrorKind::TimedOut,
                    "SSH upload-pack stdin shutdown timed out",
                )
            })??;

        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| IoError::other("SSH child stdout not captured"))?;
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, IoError>>(32);
        let idle_timeout = self.idle_timeout;

        tokio::spawn(async move {
            let SshProcess {
                mut child,
                stderr_capture,
                ..
            } = child;

            let mut buf = [0u8; 16 * 1024];
            let mut forward_err: Option<IoError> = None;
            let mut sent_any_stdout = false;
            loop {
                let read = tokio::select! {
                    _ = tx.closed() => return,
                    result = tokio::time::timeout(idle_timeout, stdout.read(&mut buf)) => result,
                };
                match read {
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
            let stderr_buf = finish_stderr_capture(
                stderr_capture,
                tokio::time::Instant::now() + Duration::from_secs(1),
            ).await;

            if let Some(err) = forward_err {
                let _ = tx.send(Err(err)).await;
            } else if let Some(status) = status
                && !status.success()
            {
                let _ = tx
                    .send(Err(IoError::other(format!(
                        "SSH upload-pack failed: {}",
                        describe_status_with_stderr(&status, stderr_buf.as_ref())
                    ))))
                    .await;
            }
        }.with_current_subscriber());

        Ok(ReceiverStream::new(rx).boxed())
    }

    pub async fn send_pack(&self, data: Bytes) -> Result<Bytes, IoError> {
        let mut child = self.spawn_service(ServiceType::ReceivePack).await?;
        let advertisement = {
            let stdout = child
                .child
                .stdout
                .as_mut()
                .ok_or_else(|| IoError::other("SSH child stdout not captured"))?;
            self.read_advertisement(stdout, &mut child.stdout_observed)
                .await
        };
        if let Err(read_err) = advertisement {
            return Err(
                finish_ssh_read_error(child, read_err, "SSH advertisement read failed").await,
            );
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
        let output = tokio::time::timeout(
            timeout,
            child.collect_output(
                tokio::time::Instant::now() + timeout,
                SSH_PROTOCOL_OUTPUT_LIMIT,
            ),
        )
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
        if output.stdout.total > SSH_PROTOCOL_OUTPUT_LIMIT as u64 {
            return Err(IoError::other(
                "SSH receive-pack response exceeded the 16 MiB limit; push fewer refs and retry",
            ));
        }
        Ok(Bytes::from(output.stdout.bytes))
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

#[derive(Debug)]
struct SshProtocolReadExit {
    source: IoError,
    code: i32,
}

impl std::fmt::Display for SshProtocolReadExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}; SSH exited with status {}; check SSH connectivity, trusted host keys, ssh-agent authentication (load or unlock the key first), and remote repository access",
            self.source, self.code
        )
    }
}

impl std::error::Error for SshProtocolReadExit {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Preserve the protocol carrier before adding any SSH or cleanup diagnostics.
/// All inner reads and outer advertisement failures share this formatter.
fn wrap_ssh_read_error(
    read_error: IoError,
    context: &'static str,
    output: Option<Result<SshProcessOutput, IoError>>,
) -> IoError {
    if is_pkt_line_io_error(&read_error)
        && let Some(Ok(output)) = &output
    {
        trace_ssh_output_metadata(&output.status, output.stderr.as_ref());
    }
    let header_eof = read_error
        .get_ref()
        .and_then(|error| error.downcast_ref::<PktLineError>())
        == Some(&PktLineError::TruncatedHeader);
    if header_eof
        && let Some(Ok(output)) = &output
        && !output.stdout_observed
        && let Some(kind) = ssh_host_key_unconfirmed(&output.status, output.stderr.as_ref())
    {
        return IoError::other(kind);
    }
    if is_pkt_line_io_error(&read_error) {
        if let Some(Ok(output)) = &output
            && let Some(code) = output.status.code()
            && code != 0
        {
            return IoError::new(
                read_error.kind(),
                SshProtocolReadExit {
                    source: read_error,
                    code,
                },
            );
        }
        return read_error;
    }
    match output {
        None => IoError::other(format!("{context}: {read_error}")),
        Some(Ok(output)) => IoError::other(format!(
            "{context}: {read_error}; {}",
            describe_process_output(&output)
        )),
        Some(Err(error)) => IoError::other(format!(
            "{context}: {read_error}; unable to collect process output: {error}"
        )),
    }
}

/// Bound the entire direct-child cleanup, including a short header-EOF window
/// for SSH's own exit status. Other read failures request termination immediately.
async fn finish_ssh_read_error(
    mut child: SshProcess,
    read_error: IoError,
    context: &'static str,
) -> IoError {
    let deadline = tokio::time::Instant::now() + SSH_READ_ERROR_REAP_TIMEOUT;
    let header_eof = read_error
        .get_ref()
        .and_then(|error| error.downcast_ref::<PktLineError>())
        == Some(&PktLineError::TruncatedHeader);
    let mut cleanup_error = None;
    let exited = if header_eof {
        let status_deadline =
            (tokio::time::Instant::now() + SSH_HEADER_EOF_STATUS_TIMEOUT).min(deadline);
        match tokio::time::timeout_at(status_deadline, child.wait()).await {
            Ok(Ok(_)) => true,
            Ok(Err(error)) => {
                cleanup_error = Some(IoError::other(format!(
                    "unable to read SSH exit status: {error}"
                )));
                false
            }
            Err(_) => false,
        }
    } else {
        false
    };
    if !exited && let Err(error) = child.start_kill() {
        let detail = format!("unable to stop SSH child: {error}");
        cleanup_error = Some(match cleanup_error {
            Some(previous) => IoError::other(format!("{previous}; {detail}")),
            None => IoError::other(detail),
        });
    }
    if is_pkt_line_io_error(&read_error) {
        // Discard stdout; retain only bounded stderr for metadata and local
        // host-trust classification, subject to the same cleanup deadline.
        drop(child.stdout.take());
    }
    let output = match tokio::time::timeout_at(deadline, child.collect_output(deadline, 0)).await {
        Ok(output) => output,
        Err(_) => Err(IoError::new(
            ErrorKind::TimedOut,
            "SSH child cleanup exceeded the two-second limit",
        )),
    };
    // The primary protocol error survives even a cleanup failure. The child is
    // configured with kill_on_drop as a fallback; no remote output is rendered.
    finish_ssh_read_result(read_error, context, output, cleanup_error)
}

fn finish_ssh_read_result(
    read_error: IoError,
    context: &'static str,
    output: Result<SshProcessOutput, IoError>,
    cleanup_error: Option<IoError>,
) -> IoError {
    let error = wrap_ssh_read_error(read_error, context, Some(output));
    if let Some(cleanup_error) = cleanup_error
        && !is_pkt_line_io_error(&error)
        && !error
            .get_ref()
            .is_some_and(|inner| inner.is::<SshHostKeyUnconfirmed>())
    {
        // Preserve typed protocol and host-trust primary errors. An ordinary
        // failure still includes the collected status and local cleanup warning.
        return IoError::other(format!("{error}; SSH cleanup warning: {cleanup_error}"));
    }
    error
}

fn trace_ssh_output_metadata(status: &std::process::ExitStatus, stderr: Option<&SshCapturedBytes>) {
    let status_text = status.code().map_or_else(
        || "terminated by signal".to_string(),
        |code| code.to_string(),
    );
    let Some(stderr) = stderr else {
        tracing::debug!(ssh_exit_status = %status_text, stderr_available = false, "SSH process diagnostics unavailable");
        return;
    };
    tracing::debug!(ssh_exit_status = %status_text, stderr_bytes = stderr.total, stderr_retained_bytes = stderr.bytes.len(), stderr_sha256 = %hex::encode(stderr.digest), "SSH process diagnostics");
}

fn describe_process_output(output: &SshProcessOutput) -> String {
    describe_status_with_stderr(&output.status, output.stderr.as_ref())
}

fn describe_status_with_stderr(
    status: &std::process::ExitStatus,
    stderr: Option<&SshCapturedBytes>,
) -> String {
    let status_text = status.code().map_or_else(
        || "terminated by signal".to_string(),
        |code| code.to_string(),
    );
    trace_ssh_output_metadata(status, stderr);
    if status.code() == Some(255) {
        format!(
            "exit status {status_text}; SSH diagnostics withheld; check connectivity and repository access, and load or unlock the key in ssh-agent before retrying"
        )
    } else {
        format!("exit status {status_text}; SSH diagnostics withheld")
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
pub(crate) mod tests {
    use super::*;

    const PKT11_SENTINEL: &str = "PKT11_REMOTE_SECRET_8dcbf3\x1b[31m\rspoof";

    #[derive(Clone, Default)]
    struct Pkt11Trace(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Pkt11Trace {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Pkt11Trace {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }
    impl Pkt11Trace {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
        fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync + 'static {
            tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_max_level(tracing::Level::DEBUG)
                .with_writer(self.clone())
                .finish()
        }
    }

    #[test]
    fn pkt_line_client_describe_status_no_stderr() {
        let mut output = pkt12_output_with_code(23);
        output.stderr = Some(SshCapturedBytes::from_fixture(
            PKT11_SENTINEL.as_bytes(),
            SSH_STDERR_LIMIT,
        ));
        assert_eq!(
            describe_status_with_stderr(&output.status, output.stderr.as_ref()),
            "exit status 23; SSH diagnostics withheld"
        );
        assert!(!format!("{output:?}").contains("PKT11_REMOTE_SECRET"));
        let mut native = pkt12_output_with_code(255);
        assert_eq!(
            describe_process_output(&native),
            "exit status 255; SSH diagnostics withheld; check connectivity and repository access, and load or unlock the key in ssh-agent before retrying"
        );
        native.stderr = None;
        let trace = Pkt11Trace::default();
        tracing::subscriber::with_default(trace.subscriber(), || {
            assert_eq!(
                describe_process_output(&native),
                "exit status 255; SSH diagnostics withheld; check connectivity and repository access, and load or unlock the key in ssh-agent before retrying"
            );
        });
        let logs = trace.text();
        assert!(logs.contains("stderr_available=false"));
        assert!(!logs.contains("stderr_sha256") && !logs.contains("stderr_bytes"));
    }

    #[test]
    fn pkt_line_client_describe_process_output_no_stderr() {
        let mut output = pkt12_output_with_code(23);
        output.stderr = Some(SshCapturedBytes::from_fixture(
            PKT11_SENTINEL.as_bytes(),
            SSH_STDERR_LIMIT,
        ));
        output.stdout =
            SshCapturedBytes::from_fixture(PKT11_SENTINEL.as_bytes(), SSH_PROTOCOL_OUTPUT_LIMIT);
        assert_eq!(
            describe_process_output(&output),
            "exit status 23; SSH diagnostics withheld"
        );
        assert!(!format!("{output:?}").contains("PKT11_REMOTE_SECRET"));
    }

    #[test]
    fn pkt_line_client_debug_trace_records_status_length_digest() {
        let trace = Pkt11Trace::default();
        let mut output = pkt12_output_with_code(23);
        output.stderr = Some(SshCapturedBytes::from_fixture(
            PKT11_SENTINEL.as_bytes(),
            SSH_STDERR_LIMIT,
        ));
        tracing::subscriber::with_default(trace.subscriber(), || describe_process_output(&output));
        let text = trace.text();
        assert!(text.contains("ssh_exit_status=23"), "{text}");
        assert!(
            text.contains(&format!("stderr_bytes={}", PKT11_SENTINEL.len())),
            "{text}"
        );
        assert!(
            text.contains(&format!("stderr_retained_bytes={}", PKT11_SENTINEL.len())),
            "{text}"
        );
        assert!(
            text.contains(&hex::encode(sha2::Sha256::digest(
                PKT11_SENTINEL.as_bytes()
            ))),
            "{text}"
        );
        assert!(!text.contains("PKT11_REMOTE_SECRET"));
        assert!(!text.contains('\x1b'));
        assert!(!text.contains("spoof"));
    }

    #[cfg(unix)]
    struct Pkt11Fixture {
        _root: tempfile::TempDir,
        script: std::path::PathBuf,
        arguments: std::path::PathBuf,
        pid: std::path::PathBuf,
    }
    #[cfg(unix)]
    impl Pkt11Fixture {
        fn new(
            advertisement: &[u8],
            complete: bool,
            stderr: &[u8],
            stdout: &[u8],
            code: i32,
        ) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let root = tempfile::tempdir().unwrap();
            let script = root.path().join("ssh");
            let arguments = root.path().join("arguments");
            let pid = root.path().join("pid");
            let ad = root.path().join("advertisement");
            let err = root.path().join("stderr");
            let out = root.path().join("stdout");
            std::fs::write(&ad, advertisement).unwrap();
            std::fs::write(&err, stderr).unwrap();
            std::fs::write(&out, stdout).unwrap();
            let q = |p: &std::path::Path| shell_single_quote(p.to_str().unwrap());
            let completion = if complete {
                format!("cat >/dev/null\ncat {}\nexit {code}", q(&out))
            } else {
                format!("exit {code}")
            };
            let text = format!(
                "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > {}\nprintf '%s\\n' \"$$\" > {}\ncat {} >&2\ncat {}\n{completion}\n",
                q(&arguments),
                q(&pid),
                q(&err),
                q(&ad)
            );
            std::fs::write(&script, text).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            Self {
                _root: root,
                script,
                arguments,
                pid,
            }
        }
        fn assert_reaped(&self) {
            let pid = std::fs::read_to_string(&self.pid)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            pkt12_assert_reaped(pid);
        }
    }

    #[cfg(unix)]
    async fn pkt11_run_client(phase: &str, fixture: &Pkt11Fixture) -> (String, String) {
        use crate::utils::test::ScopedEnvVar;
        let _ssh = ScopedEnvVar::set("LIBRA_SSH_COMMAND", &fixture.script);
        let client = SshClient::from_ssh_spec("git@fixture.invalid:repo")
            .unwrap()
            .with_idle_timeout(Duration::from_secs(2));
        let trace = Pkt11Trace::default();
        let error = tokio::time::timeout(
            Duration::from_secs(10),
            async {
                match phase {
                    "discovery" => client
                        .discovery_reference(ServiceType::UploadPack)
                        .await
                        .unwrap_err()
                        .to_string(),
                    "fetch" => match client
                        .fetch_objects(
                            &[],
                            &["1111111111111111111111111111111111111111".to_string()],
                            &[],
                            None,
                        )
                        .await
                    {
                        Err(error) => error.to_string(),
                        Ok(mut stream) => {
                            let mut errors = Vec::new();
                            while let Some(item) = stream.next().await {
                                if let Err(error) = item {
                                    errors.push(error.to_string());
                                }
                            }
                            assert_eq!(errors.len(), 1, "fetch status must fail exactly once");
                            errors.remove(0)
                        }
                    },
                    "push" => client
                        .send_pack(Bytes::from_static(b"0000"))
                        .await
                        .unwrap_err()
                        .to_string(),
                    _ => unreachable!(),
                }
            }
            .with_subscriber(trace.subscriber()),
        )
        .await
        .expect("SSH fixture must terminate within its local budget");
        fixture.assert_reaped();
        assert!(!error.contains("PKT11_REMOTE_SECRET"), "{phase}: {error}");
        assert!(!error.contains('\x1b'), "{phase}: {error}");
        let logs = trace.text();
        assert!(!logs.contains("PKT11_REMOTE_SECRET"), "{phase}: {logs}");
        assert!(!logs.contains('\x1b'), "{phase}: {logs}");
        (error, logs)
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(env)]
    async fn pkt_line_client_nonzero_exit_discovery_zero_stderr() {
        // Exit after the advertisement without waiting for a request: this is
        // the native nonzero discovery branch, rather than a malformed reader.
        let fixture = Pkt11Fixture::new(b"0000", false, PKT11_SENTINEL.as_bytes(), b"", 23);
        let (error, _) = pkt11_run_client("discovery", &fixture).await;
        assert!(
            error
                .contains("SSH discovery command failed: exit status 23; SSH diagnostics withheld"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(env)]
    async fn pkt_line_client_status_task_zero_stderr() {
        let fixture = Pkt11Fixture::new(
            b"0000",
            true,
            PKT11_SENTINEL.as_bytes(),
            b"PACK-fixture",
            23,
        );
        let (error, _) = pkt11_run_client("fetch", &fixture).await;
        assert!(
            error.contains("SSH upload-pack failed: exit status 23; SSH diagnostics withheld"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(env)]
    async fn pkt_line_client_send_pack_zero_stderr() {
        let fixture = Pkt11Fixture::new(
            b"0000",
            true,
            PKT11_SENTINEL.as_bytes(),
            PKT11_SENTINEL.as_bytes(),
            23,
        );
        let (error, _) = pkt11_run_client("push", &fixture).await;
        assert!(
            error.contains("SSH receive-pack failed: exit status 23; SSH diagnostics withheld"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(env)]
    async fn pkt_line_client_malicious_stderr_sentinel() {
        for phase in ["discovery", "fetch", "push"] {
            let fixture = Pkt11Fixture::new(b"0001", false, PKT11_SENTINEL.as_bytes(), b"", 23);
            let (error, _) = pkt11_run_client(phase, &fixture).await;
            assert!(
                error.contains(crate::git_protocol::PKT_LINE_PROTOCOL_ERROR_PREFIX),
                "{phase}: {error}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(env)]
    async fn pkt_line_client_batch_mode_enforced() {
        for service in [ServiceType::UploadPack, ServiceType::ReceivePack] {
            for mode in ["ask", "yes", "accept-new", "no"] {
                use crate::utils::test::ScopedEnvVar;
                let fixture = Pkt11Fixture::new(b"", false, b"", b"", 23);
                let _ssh = ScopedEnvVar::set("LIBRA_SSH_COMMAND", &fixture.script);
                let client = SshClient::from_ssh_spec("git@fixture.invalid:repo")
                    .unwrap()
                    .with_strict_host_key_checking(mode.to_string())
                    .unwrap();
                let child = client.spawn_service(service).await.unwrap();
                let output = child
                    .collect_output(tokio::time::Instant::now() + Duration::from_secs(5), 0)
                    .await
                    .unwrap();
                assert_eq!(output.status.code(), Some(23));
                let args = std::fs::read_to_string(&fixture.arguments).unwrap();
                let args = args.lines().collect::<Vec<_>>();
                assert_eq!(&args[..2], &["-o", "BatchMode=yes"]);
                assert_eq!(
                    args.iter().filter(|a| a.starts_with("BatchMode=")).count(),
                    1
                );
                assert_eq!(
                    args.iter().any(|a| a.starts_with("StrictHostKeyChecking=")),
                    mode != "ask"
                );
                if mode != "ask" {
                    assert!(args.contains(&format!("StrictHostKeyChecking={mode}").as_str()));
                }
                fixture.assert_reaped();
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(env, cwd, hash_kind)]
    async fn pkt_line_client_host_key_fail_closed_guidance() {
        use clap::Parser;

        use crate::{
            command::clone,
            utils::{
                error::StableErrorCode,
                output::OutputConfig,
                test::{ChangeDirGuard, ScopedEnvVar, setup_with_new_libra_in},
            },
        };
        for (wire, kind, message, guidance, signal) in [
            (
                "Host key verification failed",
                SshHostKeyUnconfirmed::Untrusted,
                "SSH host key could not be verified",
                SSH_HOST_KEY_GUIDANCE,
                SSH_HOST_KEY_UNCONFIRMED_SIGNAL,
            ),
            (
                "REMOTE HOST IDENTIFICATION HAS CHANGED; Host key verification failed",
                SshHostKeyUnconfirmed::Changed,
                "SSH host identity has changed",
                SSH_HOST_KEY_CHANGED_GUIDANCE,
                SSH_HOST_KEY_CHANGED_SIGNAL,
            ),
        ] {
            let mut output = pkt12_output_with_code(255);
            output.stderr = Some(SshCapturedBytes::from_fixture(
                format!("{wire} {PKT11_SENTINEL}").as_bytes(),
                SSH_STDERR_LIMIT,
            ));
            assert_eq!(
                ssh_host_key_unconfirmed(&output.status, output.stderr.as_ref()),
                Some(kind)
            );
            assert_eq!(kind.to_string(), format!("{message}; {guidance}"));
            assert_eq!(
                describe_process_output(&output),
                "exit status 255; SSH diagnostics withheld; check connectivity and repository access, and load or unlock the key in ssh-agent before retrying"
            );
            let error = finish_ssh_read_result(
                pkt12_typed_error(),
                "SSH read failed",
                Ok(output),
                Some(IoError::other("fixture cleanup warning")),
            );
            assert_eq!(
                error
                    .get_ref()
                    .and_then(|inner| inner.downcast_ref::<SshHostKeyUnconfirmed>()),
                Some(&kind)
            );
            let GitError::NetworkError(detail) = ssh_discovery_read_error(error) else {
                panic!("expected network carrier")
            };
            assert_eq!(detail, format!("{signal}{message}; {guidance}"));
            assert!(!detail.contains("PKT11_REMOTE_SECRET") && !detail.contains("cleanup warning"));
        }
        for pattern in [
            "Host key verification failed",
            "REMOTE HOST IDENTIFICATION HAS CHANGED",
        ] {
            let malicious = format!("{pattern}; {PKT11_SENTINEL}");
            let mut output = pkt12_output_with_code(255);
            output.stdout_observed = true;
            output.stderr = Some(SshCapturedBytes::from_fixture(
                malicious.as_bytes(),
                SSH_STDERR_LIMIT,
            ));
            let error =
                wrap_ssh_read_error(pkt12_typed_error(), "SSH read failed", Some(Ok(output)));
            assert!(is_pkt_line_io_error(&error));
            assert!(
                error
                    .get_ref()
                    .is_some_and(|inner| inner.is::<SshProtocolReadExit>())
            );
            assert!(!error.to_string().contains("known_hosts"));

            for stdout_before_eof in [b"0".as_slice(), b"0004"] {
                let fixture =
                    Pkt11Fixture::new(stdout_before_eof, false, malicious.as_bytes(), b"", 255);
                let (error, _) = pkt11_run_client("discovery", &fixture).await;
                assert!(
                    error.contains(crate::git_protocol::PKT_LINE_PROTOCOL_ERROR_PREFIX),
                    "{error}"
                );
                assert!(!error.contains("known_hosts"));
                assert!(!error.contains(SSH_HOST_KEY_UNCONFIRMED_SIGNAL));
                assert!(!error.contains(SSH_HOST_KEY_CHANGED_SIGNAL));
            }
            for phase in ["discovery", "fetch", "push"] {
                let fixture = Pkt11Fixture::new(
                    b"0000",
                    phase != "discovery",
                    malicious.as_bytes(),
                    b"PACK-fixture",
                    255,
                );
                let (error, _) = pkt11_run_client(phase, &fixture).await;
                assert!(
                    error.contains("SSH diagnostics withheld"),
                    "{phase}: {error}"
                );
                assert!(!error.contains("known_hosts"));
                assert!(!error.contains(SSH_HOST_KEY_UNCONFIRMED_SIGNAL));
                assert!(!error.contains(SSH_HOST_KEY_CHANGED_SIGNAL));
            }
        }
        let stderr = format!("Host key verification failed. {PKT11_SENTINEL}");
        for code in [23, 255] {
            let mut output = pkt12_output_with_code(code);
            output.stderr = Some(SshCapturedBytes::from_fixture(
                stderr.as_bytes(),
                SSH_STDERR_LIMIT,
            ));
            assert_eq!(
                ssh_host_key_unconfirmed(&output.status, output.stderr.as_ref()).is_some(),
                code == 255
            );
            let carrier = ssh_discovery_read_error(wrap_ssh_read_error(
                pkt12_typed_error(),
                "SSH read failed",
                Some(Ok(output)),
            ));
            let GitError::NetworkError(detail) = carrier else {
                panic!("expected network carrier")
            };
            assert_eq!(
                detail.starts_with(SSH_HOST_KEY_UNCONFIRMED_SIGNAL),
                code == 255
            );
            assert!(!detail.contains("PKT11_REMOTE_SECRET"));
        }
        for code in [23, 255] {
            let mut output = pkt12_output_with_code(code);
            output.stderr = Some(SshCapturedBytes::from_fixture(
                stderr.as_bytes(),
                SSH_STDERR_LIMIT,
            ));
            let error = finish_ssh_read_result(
                pkt12_typed_error(),
                "SSH read failed",
                Ok(output),
                Some(IoError::other("fixture cleanup warning")),
            );
            assert_eq!(
                error
                    .get_ref()
                    .is_some_and(|inner| inner.is::<SshHostKeyUnconfirmed>()),
                code == 255
            );
            let GitError::NetworkError(detail) = ssh_discovery_read_error(error) else {
                panic!("expected network carrier")
            };
            assert_eq!(
                detail.starts_with(SSH_HOST_KEY_UNCONFIRMED_SIGNAL),
                code == 255
            );
            assert!(!detail.contains("PKT11_REMOTE_SECRET"));
            assert!(!detail.contains("fixture cleanup warning"));
        }
        let repo = tempfile::tempdir().unwrap();
        setup_with_new_libra_in(repo.path()).await;
        let _cwd = ChangeDirGuard::new(repo.path());
        // Keep the current-thread runtime and call transport setup directly
        // after DB writes: a blocking nested-runtime lookup strands pool returns.
        {
            use crate::{command::fetch::RemoteClient, internal::config::ConfigKv};

            // Exercise required vault-entry and unseal reads on that same runtime.
            // Invalid ciphertext reaches decode only after the unseal lookup.
            tokio::time::timeout(Duration::from_secs(5), async {
                let key = "vault.ssh.pkt11-vault.privkey";
                ConfigKv::set(key, "not-hex", false).await.unwrap();
                let error = RemoteClient::from_spec_with_remote(
                    "git@fixture.invalid:repo",
                    Some("pkt11-vault"),
                )
                .await
                .err()
                .expect("unencrypted vault entry must be rejected");
                assert_eq!(
                    error,
                    format!("vault SSH private key '{key}' must be encrypted")
                );
                ConfigKv::set("vault.unsealkey", &"11".repeat(32), false)
                    .await
                    .unwrap();
                ConfigKv::set(key, "not-hex", true).await.unwrap();
                let error = RemoteClient::from_spec_with_remote(
                    "git@fixture.invalid:repo",
                    Some("pkt11-vault"),
                )
                .await
                .err()
                .expect("invalid ciphertext must be rejected");
                assert!(
                    error.starts_with(&format!("failed to decode vault SSH private key '{key}':")),
                    "{error}"
                );
            })
            .await
            .expect("vault configuration must not block the runtime worker");
        }
        for (wire, message, guidance) in [
            (
                stderr,
                "SSH host key could not be verified",
                SSH_HOST_KEY_GUIDANCE,
            ),
            (
                format!(
                    "REMOTE HOST IDENTIFICATION HAS CHANGED; Host key verification failed {PKT11_SENTINEL}"
                ),
                "SSH host identity has changed",
                SSH_HOST_KEY_CHANGED_GUIDANCE,
            ),
        ] {
            let fixture = Pkt11Fixture::new(b"", false, wire.as_bytes(), b"", 255);
            let _ssh = ScopedEnvVar::set("LIBRA_SSH_COMMAND", &fixture.script);
            let target = repo.path().join("clone-target");
            let args = clone::CloneArgs::try_parse_from([
                "clone",
                "git@fixture.invalid:repo",
                target.to_str().unwrap(),
            ])
            .unwrap();
            let error = tokio::time::timeout(
                Duration::from_secs(15),
                clone::execute_safe(args, &OutputConfig::default()),
            )
            .await
            .unwrap()
            .unwrap_err();
            assert_eq!(error.stable_code(), StableErrorCode::NetworkUnavailable);
            assert_eq!(error.exit_code(), 128);
            assert_eq!(error.message(), message);
            assert_eq!(error.hints().len(), 1);
            assert_eq!(error.hints()[0].as_str(), guidance);
            for output in [
                error.render(),
                error.render_report(),
                error.render_json().to_string(),
            ] {
                assert!(!output.contains("PKT11_REMOTE_SECRET"));
                assert!(!output.contains(SSH_HOST_KEY_UNCONFIRMED_SIGNAL));
                assert!(!output.contains(SSH_HOST_KEY_CHANGED_SIGNAL));
                assert!(!output.contains("ssh-keyscan"));
            }
            fixture.assert_reaped();
            let args = std::fs::read_to_string(&fixture.arguments).unwrap();
            assert_eq!(args.lines().last(), Some("git-upload-pack 'repo'"));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(env, cwd, hash_kind)]
    async fn pkt_line_client_stderr_flood_capped() {
        use clap::Parser;

        use crate::{
            command::{clone, push},
            internal::{branch::Branch, config::ConfigKv},
            utils::{
                error::StableErrorCode,
                output::OutputConfig,
                test::{ChangeDirGuard, ScopedEnvVar, setup_with_new_libra_in},
            },
        };
        let flood = PKT11_SENTINEL.as_bytes().repeat(40_000);
        assert!(flood.len() > 1024 * 1024);
        let expected_digest = hex::encode(sha2::Sha256::digest(&flood));
        for phase in ["discovery", "fetch", "push"] {
            for malformed in [true, false] {
                let ad = if malformed { b"0001" } else { b"0000" };
                let complete = !malformed && phase != "discovery";
                let fixture = Pkt11Fixture::new(ad, complete, &flood, b"PACK-fixture", 23);
                let (error, logs) = pkt11_run_client(phase, &fixture).await;
                assert!(
                    if malformed {
                        error.contains(crate::git_protocol::PKT_LINE_PROTOCOL_ERROR_PREFIX)
                    } else {
                        error.contains("SSH diagnostics withheld")
                    },
                    "{phase}: {error}"
                );
                assert!(
                    logs.contains(&format!("stderr_bytes={}", flood.len())),
                    "{phase}: {logs}"
                );
                assert!(
                    logs.contains(&format!("stderr_retained_bytes={SSH_STDERR_LIMIT}")),
                    "{phase}: {logs}"
                );
                assert!(logs.contains(&expected_digest), "{phase}: {logs}");
            }
        }
        // Check bounded retention and full-stream digest independently of stderr
        // lifecycle, including zero-retention discarded stdout.
        for limit in [0, SSH_STDERR_LIMIT, SSH_PROTOCOL_OUTPUT_LIMIT] {
            let input = vec![b'x'; limit + 17];
            let (mut writer, reader) = tokio::io::duplex(1024);
            let task = SshCaptureTask::start(reader, limit);
            let expected = input.clone();
            let write = tokio::spawn(async move {
                writer.write_all(&input).await.unwrap();
                writer.shutdown().await.unwrap();
            });
            let captured = task
                .finish(tokio::time::Instant::now() + Duration::from_secs(5))
                .await
                .unwrap();
            write.await.unwrap();
            assert_eq!(captured.bytes.len(), limit);
            assert_eq!(captured.total, expected.len() as u64);
            assert_eq!(
                captured.digest,
                <[u8; 32]>::from(sha2::Sha256::digest(&expected))
            );
        }
        let (mut writer, reader) = tokio::io::duplex(8);
        let task = SshCaptureTask::start(reader, SSH_STDERR_LIMIT);
        let abort = task.task.abort_handle();
        let error = task
            .finish(tokio::time::Instant::now() + Duration::from_millis(10))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        tokio::time::timeout(Duration::from_secs(2), async {
            while !abort.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            writer.write_all(b"x").await.is_err(),
            "timed-out collector must close its pipe"
        );
        // Model a descendant-held stderr descriptor with a test-owned open
        // pipe. The native child really exits; keeping the writer in this test
        // avoids spawning an orphan solely to delay EOF.
        for code in [0, 23] {
            let child = tokio::process::Command::new("sh")
                .args(["-c", &format!("printf '0000'; exit {code}")])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let pid = child.id().unwrap();
            let mut process = SshProcess::new(child).unwrap();
            tokio::time::timeout(Duration::from_secs(5), process.child.wait())
                .await
                .unwrap()
                .unwrap();
            let (writer, reader) = tokio::io::duplex(8);
            process.stderr_capture = SshCaptureTask::start(reader, SSH_STDERR_LIMIT);
            let output = process
                .collect_output(
                    tokio::time::Instant::now() + Duration::from_millis(50),
                    SSH_PROTOCOL_OUTPUT_LIMIT,
                )
                .await
                .unwrap();
            assert_eq!(output.status.code(), Some(code));
            assert_eq!(output.stdout.bytes, b"0000");
            assert!(output.stderr.is_none());
            assert_eq!(
                describe_process_output(&output),
                format!("exit status {code}; SSH diagnostics withheld")
            );
            parse_discovered_references(Bytes::from(output.stdout.bytes), ServiceType::UploadPack)
                .unwrap();
            drop(writer);
            pkt12_assert_reaped(pid);
        }
        let (writer, reader) = tokio::io::duplex(8);
        let capture = SshCaptureTask::start(reader, SSH_STDERR_LIMIT);
        let abort = capture.task.abort_handle();
        assert!(
            finish_stderr_capture(
                capture,
                tokio::time::Instant::now() + Duration::from_millis(10)
            )
            .await
            .is_none()
        );
        drop(writer);
        tokio::time::timeout(Duration::from_secs(2), async {
            while !abort.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let oversized = vec![b'x'; SSH_PROTOCOL_OUTPUT_LIMIT + 1];
        let fixture = Pkt11Fixture::new(b"0000", true, b"", &oversized, 0);
        let (error, _) = pkt11_run_client("push", &fixture).await;
        assert!(error.contains("response exceeded the 16 MiB limit"));
        let mut ad = Vec::new();
        while ad.len() <= SSH_PROTOCOL_OUTPUT_LIMIT {
            ad.extend_from_slice(b"ffff");
            ad.extend(std::iter::repeat_n(b'x', 65531));
        }
        let fixture = Pkt11Fixture::new(&ad, false, b"", b"", 0);
        let (error, _) = pkt11_run_client("discovery", &fixture).await;
        assert!(error.contains("SSH advertisement exceeded the 16 MiB limit; use the repository's HTTPS URL if available, or ask its maintainer to reduce refs"));

        // Exercise the real command mappings with the actual over-limit bytes.
        // The delete-only push avoids object/cloud access and must preserve the
        // local tracking ref when the remote response cannot be accepted.
        let _storage = ScopedEnvVar::set("LIBRA_STORAGE_TYPE", "local");
        let repo = tempfile::tempdir().unwrap();
        setup_with_new_libra_in(repo.path()).await;
        let _cwd = ChangeDirGuard::new(repo.path());
        let target = repo.path().join("oversized-clone");
        let clone_error = {
            let _ssh = ScopedEnvVar::set("LIBRA_SSH_COMMAND", &fixture.script);
            tokio::time::timeout(
                Duration::from_secs(15),
                clone::execute_safe(
                    clone::CloneArgs::try_parse_from([
                        "clone",
                        "git@fixture.invalid:repo",
                        target.to_str().unwrap(),
                    ])
                    .unwrap(),
                    &OutputConfig::default(),
                ),
            )
            .await
            .unwrap()
            .unwrap_err()
        };
        fixture.assert_reaped();
        assert!(
            clone_error
                .message()
                .contains("SSH advertisement exceeded the 16 MiB limit; use the repository's HTTPS URL if available, or ask its maintainer to reduce refs")
        );
        let oid = "1111111111111111111111111111111111111111";
        let tracking = "refs/remotes/origin/main";
        ConfigKv::set("remote.origin.url", "git@fixture.invalid:repo", false)
            .await
            .unwrap();
        Branch::update_branch(tracking, oid, Some("origin"))
            .await
            .unwrap();
        let reference = format!("{oid} refs/heads/main\0report-status delete-refs\n");
        let advertisement = format!("{:04x}{reference}0000", reference.len() + 4);
        let fixture = Pkt11Fixture::new(advertisement.as_bytes(), true, b"", &oversized, 0);
        let push_error = {
            let _ssh = ScopedEnvVar::set("LIBRA_SSH_COMMAND", &fixture.script);
            tokio::time::timeout(
                Duration::from_secs(15),
                push::execute_safe(
                    push::PushArgs::try_parse_from(["push", "origin", ":refs/heads/main"]).unwrap(),
                    &OutputConfig::default(),
                ),
            )
            .await
            .unwrap()
            .unwrap_err()
        };
        fixture.assert_reaped();
        assert!(
            push_error
                .message()
                .contains("response exceeded the 16 MiB limit"),
            "{push_error:?}"
        );
        assert_eq!(
            Branch::find_branch_result(tracking, Some("origin"))
                .await
                .unwrap()
                .unwrap()
                .commit
                .to_string(),
            oid
        );
        for (error, hint) in [
            (
                clone_error,
                "check the remote host, DNS, VPN/proxy, and network connectivity",
            ),
            (push_error, "check network connectivity and retry"),
        ] {
            assert_eq!(error.stable_code(), StableErrorCode::NetworkUnavailable);
            assert_eq!(error.exit_code(), 128);
            assert_eq!(
                error.hints().iter().map(|h| h.as_str()).collect::<Vec<_>>(),
                [hint]
            );
            for rendered in [error.render(), error.render_report(), error.render_json()] {
                assert!(!rendered.contains(PKT11_SENTINEL));
                assert!(!rendered.contains("xxxxxxxxxxxxxxxx"));
            }
            let report: serde_json::Value = serde_json::from_str(&error.render_json()).unwrap();
            assert_eq!(report["error_code"], "LBR-NET-001");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(env)]
    async fn pkt_line_client_interactive_stderr_captured_sanitized() {
        use std::{io::IsTerminal, os::fd::FromRawFd};
        const CHILD: &str = "LIBRA_PKT11_PTY_CHILD";
        if std::env::var_os(CHILD).as_deref() == Some(std::ffi::OsStr::new("1")) {
            assert!(
                std::io::stdin().is_terminal(),
                "child must have actual terminal stdin"
            );
            let fixture = Pkt11Fixture::new(b"", false, PKT11_SENTINEL.as_bytes(), b"", 23);
            let _ = pkt11_run_client("discovery", &fixture).await;
            let args = std::fs::read_to_string(&fixture.arguments).unwrap();
            assert!(args.contains("BatchMode=yes"));
            return;
        }
        let mut master = -1;
        let mut slave = -1;
        // SAFETY: both descriptors are writable integers; optional name,
        // termios and window-size pointers are null as allowed by openpty.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        // SAFETY: successful openpty transferred two distinct owned descriptors.
        let master = unsafe { std::fs::File::from_raw_fd(master) };
        let slave = unsafe { std::fs::File::from_raw_fd(slave) };
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", "internal::protocol::ssh_client::tests::pkt_line_client_interactive_stderr_captured_sanitized", "--nocapture", "--test-threads=1"])
            .env(CHILD, "1").stdin(slave).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(30), child.output())
            .await
            .expect("terminal child test must finish")
            .unwrap();
        drop(master);
        assert!(
            output.status.success(),
            "terminal child test failed with {:?}",
            output.status.code()
        );
        for bytes in [&output.stdout, &output.stderr] {
            let text = String::from_utf8_lossy(bytes);
            assert!(
                !text.contains("PKT11_REMOTE_SECRET"),
                "remote sentinel reached inherited terminal output"
            );
            assert!(
                !text.contains('\x1b'),
                "remote terminal control sequence escaped capture"
            );
        }
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed;"));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(env)]
    async fn pkt_line_client_batch_mode_passphrase_error_points_to_agent() {
        use crate::utils::test::ScopedEnvVar;
        let root = tempfile::tempdir().unwrap();
        let key = root.path().join("encrypted_ed25519");
        let output = tokio::time::timeout(
            Duration::from_secs(15),
            tokio::process::Command::new("ssh-keygen")
                .args([
                    "-q",
                    "-t",
                    "ed25519",
                    "-N",
                    "pkt11-fixture-passphrase",
                    "-f",
                ])
                .arg(&key)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .expect("the Unix SSH regression requires OpenSSH ssh-keygen");
        assert!(output.status.success());
        // This fixture contains a real encrypted key, no agent, and a simulated
        // SSH exit255 after verifying that an empty passphrase cannot unlock it.
        // It tests our process/guidance contract, not an actual network handshake.
        let fixture = Pkt11Fixture::new(
            b"",
            false,
            format!("Permission denied (publickey). {PKT11_SENTINEL}").as_bytes(),
            b"",
            255,
        );
        let original = std::fs::read_to_string(&fixture.script).unwrap();
        let key_arg = shell_single_quote(key.to_str().unwrap());
        let checks = format!(
            "test -z \"${{SSH_AUTH_SOCK-}}\"\nfound_key=no\nfor arg; do if [ \"$arg\" = {key_arg} ]; then found_key=yes; fi; done\ntest \"$found_key\" = yes\nif ssh-keygen -y -P '' -f {key_arg} >/dev/null 2>&1; then exit 91; fi\n"
        );
        std::fs::write(
            &fixture.script,
            original.replacen("set -eu\n", &format!("set -eu\n{checks}"), 1),
        )
        .unwrap();
        let _ssh = ScopedEnvVar::set("LIBRA_SSH_COMMAND", &fixture.script);
        let _agent = ScopedEnvVar::unset("SSH_AUTH_SOCK");
        let client = SshClient::from_ssh_spec("git@fixture.invalid:repo")
            .unwrap()
            .with_key_path(key.to_str().unwrap().to_string());
        let error = tokio::time::timeout(
            Duration::from_secs(10),
            client.discovery_reference(ServiceType::UploadPack),
        )
        .await
        .unwrap()
        .unwrap_err()
        .to_string();
        assert!(error.contains("SSH exited with status 255"), "{error}");
        assert!(
            error.contains("ssh-agent") && error.contains("load or unlock the key"),
            "{error}"
        );
        assert!(!error.contains("Permission denied (publickey)"));
        assert!(!error.contains("PKT11_REMOTE_SECRET"));
        assert!(!error.contains("pkt11-fixture-passphrase"));
        fixture.assert_reaped();
        let args = std::fs::read_to_string(&fixture.arguments).unwrap();
        assert!(args.contains("BatchMode=yes"));
        assert!(args.lines().any(|arg| arg == "-i"));
    }

    const PKT12_SENTINEL: &str = "PKT12_REMOTE_SECRET_0ec451";

    fn pkt12_output() -> SshProcessOutput {
        pkt12_output_with_code(23)
    }

    fn pkt12_output_with_code(code: i32) -> SshProcessOutput {
        #[cfg(unix)]
        let status = {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(code << 8)
        };
        #[cfg(windows)]
        let status = {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(code as u32)
        };
        SshProcessOutput {
            stdout_observed: false,
            status,
            stdout: SshCapturedBytes::from_fixture(
                PKT12_SENTINEL.as_bytes(),
                SSH_PROTOCOL_OUTPUT_LIMIT,
            ),
            stderr: Some(SshCapturedBytes::from_fixture(
                PKT12_SENTINEL.as_bytes(),
                SSH_STDERR_LIMIT,
            )),
        }
    }

    fn pkt12_typed_error() -> IoError {
        IoError::new(ErrorKind::InvalidData, PktLineError::TruncatedHeader)
    }

    fn pkt12_assert_wrapper(context: &'static str) {
        for output in [
            Ok(pkt12_output_with_code(0)),
            Ok(pkt12_output()),
            Err(IoError::other(format!("collect failed: {PKT12_SENTINEL}"))),
        ] {
            let exit_code = output.as_ref().ok().and_then(|output| output.status.code());
            let error = wrap_ssh_read_error(pkt12_typed_error(), context, Some(output));
            assert_eq!(error.kind(), ErrorKind::InvalidData);
            let original = if exit_code == Some(23) {
                let cause = error
                    .get_ref()
                    .unwrap()
                    .downcast_ref::<SshProtocolReadExit>()
                    .unwrap();
                assert_eq!(cause.code, 23);
                assert!(
                    error
                        .to_string()
                        .starts_with(&PktLineError::TruncatedHeader.to_string())
                );
                assert!(error.to_string().contains("SSH exited with status 23"));
                assert!(error.to_string().contains("ssh-agent authentication"));
                &cause.source
            } else {
                assert_eq!(error.to_string(), PktLineError::TruncatedHeader.to_string());
                &error
            };
            assert_eq!(
                original
                    .get_ref()
                    .and_then(|e| e.downcast_ref::<PktLineError>()),
                Some(&PktLineError::TruncatedHeader)
            );
            assert!(!error.to_string().contains(PKT12_SENTINEL));
        }
    }

    #[test]
    fn pkt_line_client_ssh_wrapper_marker_passthrough_discovery() {
        pkt12_assert_wrapper("SSH read failed");
        for output in [Ok(pkt12_output()), Err(IoError::other(PKT12_SENTINEL))] {
            let error = wrap_ssh_read_error(pkt12_typed_error(), "SSH read failed", Some(output));
            let expected = error.to_string();
            let carrier = GitError::NetworkError(expected.clone());
            assert!(matches!(carrier, GitError::NetworkError(ref detail)
                if detail == &expected));
        }
    }

    #[test]
    fn pkt_line_client_ssh_wrapper_passthrough_fetch_objects() {
        pkt12_assert_wrapper("SSH advertisement read failed");
    }

    #[test]
    fn pkt_line_client_ssh_wrapper_passthrough_send_pack() {
        pkt12_assert_wrapper("SSH advertisement read failed");
    }

    #[tokio::test]
    async fn pkt_line_client_non_marker_wrapped_regression() {
        let ordinary = || IoError::new(ErrorKind::ConnectionReset, "connection reset fixture");
        assert_eq!(
            wrap_ssh_read_error(ordinary(), "SSH read failed", None).to_string(),
            "SSH read failed: connection reset fixture"
        );
        for context in ["SSH read failed", "SSH advertisement read failed"] {
            assert_eq!(
                wrap_ssh_read_error(ordinary(), context, Some(Ok(pkt12_output()))).to_string(),
                format!(
                    "{context}: connection reset fixture; exit status 23; SSH diagnostics withheld"
                )
            );
            assert_eq!(
                wrap_ssh_read_error(
                    ordinary(),
                    context,
                    Some(Err(IoError::other("fixture wait failure")))
                )
                .to_string(),
                format!(
                    "{context}: connection reset fixture; unable to collect process output: fixture wait failure"
                )
            );
            let collected = finish_ssh_read_result(
                ordinary(),
                context,
                Ok(pkt12_output()),
                Some(IoError::other("fixture kill denied")),
            );
            assert_eq!(
                collected.to_string(),
                format!(
                    "{context}: connection reset fixture; exit status 23; SSH diagnostics withheld; SSH cleanup warning: fixture kill denied"
                )
            );
            let failed = finish_ssh_read_result(
                ordinary(),
                context,
                Err(IoError::other("fixture wait failure")),
                Some(IoError::other("fixture kill denied")),
            );
            assert!(failed.to_string().contains("fixture wait failure"));
            assert!(failed.to_string().contains("fixture kill denied"));
        }

        #[cfg(unix)]
        {
            let mut child = pkt12_fault_child(b"0001").await;
            let pid = child.id().unwrap();
            // Reading the fixture's frame confirms its stderr was written and
            // the still-running process is ready before injecting ordinary IO.
            let client = SshClient::from_ssh_spec("git@fixture.invalid:repo").unwrap();
            tokio::time::timeout(
                Duration::from_secs(5),
                client.read_advertisement(
                    child.child.stdout.as_mut().unwrap(),
                    &mut child.stdout_observed,
                ),
            )
            .await
            .expect("ordinary-error fixture becomes ready")
            .unwrap_err();
            assert!(child.try_wait().unwrap().is_none());
            let error = tokio::time::timeout(
                Duration::from_secs(5),
                finish_ssh_read_error(child, ordinary(), "SSH read failed"),
            )
            .await
            .expect("ordinary read-error cleanup is bounded");
            assert!(
                error
                    .to_string()
                    .starts_with("SSH read failed: connection reset fixture; ")
            );
            assert!(error.to_string().contains("terminated by signal"));
            assert!(!error.to_string().contains(PKT12_SENTINEL));
            assert!(!is_pkt_line_io_error(&error));
            pkt12_assert_reaped(pid);
        }
    }

    #[test]
    fn pkt_line_client_zero_echo_sentinel() {
        for context in ["SSH read failed", "SSH advertisement read failed"] {
            pkt12_assert_wrapper(context);
        }
        // Actual malicious child stderr is additionally exercised by the existing
        // end-to-end named gate below; helper inputs are not that proof.
    }

    #[test]
    fn pkt_line_client_marker_at_string_start() {
        use crate::git_protocol::PKT_LINE_PROTOCOL_ERROR_PREFIX;
        for context in ["SSH read failed", "SSH advertisement read failed"] {
            let error = wrap_ssh_read_error(pkt12_typed_error(), context, Some(Ok(pkt12_output())));
            assert!(
                error
                    .to_string()
                    .starts_with(PKT_LINE_PROTOCOL_ERROR_PREFIX)
            );
            let lookalike = IoError::other(format!(
                "context: {PKT_LINE_PROTOCOL_ERROR_PREFIX}lookalike"
            ));
            let error = wrap_ssh_read_error(lookalike, context, None);
            assert!(error.to_string().starts_with(context));
        }
    }

    struct Pkt12InjectedReader {
        prefix: &'static [u8],
    }

    impl AsyncRead for Pkt12InjectedReader {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            if self.prefix.is_empty() {
                return std::task::Poll::Ready(Err(pkt12_typed_error()));
            }
            let count = self.prefix.len().min(buf.remaining());
            buf.put_slice(&self.prefix[..count]);
            self.prefix = &self.prefix[count..];
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn pkt_line_client_ssh_inner_wrapper_marker_passthrough() {
        for prefix in [b"".as_slice(), b"0005"] {
            let error =
                read_test_stream(&mut Pkt12InjectedReader { prefix }, Duration::from_secs(1))
                    .await
                    .unwrap_err();
            assert_eq!(error.to_string(), PktLineError::TruncatedHeader.to_string());
            assert_eq!(error.kind(), ErrorKind::InvalidData);
        }
    }

    #[test]
    fn pkt_line_client_shared_helper_single_source() {
        let source = include_str!("ssh_client.rs");
        let production = source.split("pub(crate) mod tests {").next().unwrap();
        assert_eq!(production.matches("fn wrap_ssh_read_error(").count(), 1);
        assert_eq!(production.matches("fn finish_ssh_read_error(").count(), 1);
        assert_eq!(production.matches("wrap_ssh_read_error(").count(), 4);
        assert_eq!(
            production
                .matches("finish_ssh_read_error(child, read_err,")
                .count(),
            3
        );
    }

    #[cfg(unix)]
    async fn pkt12_fault_child(wire: &[u8]) -> SshProcess {
        // Octal format escapes encode only fixed fixture bytes. No remote text is
        // interpreted as shell syntax, and exec leaves a single direct child PID.
        let encoded = wire
            .iter()
            .map(|b| format!("\\{b:03o}"))
            .collect::<String>();
        let child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "printf '%s' '{PKT12_SENTINEL}' >&2; printf '{encoded}'; exec 1>&-; exec sleep 30"
            ))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        SshProcess::new(child).unwrap()
    }

    #[cfg(unix)]
    fn pkt12_assert_reaped(pid: u32) {
        let mut status = 0;
        // SAFETY: waitpid receives a writable status pointer and the exact PID of
        // this test's child; WNOHANG cannot wait on an unrelated process.
        let result = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
        assert_eq!(result, -1, "direct SSH child should already be reaped");
        assert_eq!(IoError::last_os_error().raw_os_error(), Some(libc::ECHILD));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pkt_line_client_ssh_read_error_reaps_child() {
        let mut child = pkt12_fault_child(b"0008abc").await;
        let pid = child.id().unwrap();
        let client = SshClient::from_ssh_spec("git@fixture.invalid:repo").unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            client.read_advertisement(
                child.child.stdout.as_mut().unwrap(),
                &mut child.stdout_observed,
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            PktLineError::TruncatedPayload.to_string()
        );
        assert!(child.try_wait().unwrap().is_none());
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            finish_ssh_read_error(child, error, "SSH read failed"),
        )
        .await
        .expect("direct child cleanup bounded");
        assert_eq!(
            error.to_string(),
            PktLineError::TruncatedPayload.to_string()
        );
        assert!(!error.to_string().contains(PKT12_SENTINEL));
        pkt12_assert_reaped(pid);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pkt_line_client_ssh_read_error_no_hang() {
        let mut child = pkt12_fault_child(b"0001").await;
        let pid = child.id().unwrap();
        let client = SshClient::from_ssh_spec("git@fixture.invalid:repo").unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            client.read_advertisement(
                child.child.stdout.as_mut().unwrap(),
                &mut child.stdout_observed,
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(
            child.try_wait().unwrap().is_none(),
            "malformed peer still running"
        );
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            finish_ssh_read_error(child, error, "SSH advertisement read failed"),
        )
        .await
        .expect("must not wait for the 30-second sleeper");
        assert!(is_pkt_line_io_error(&error));
        assert!(!error.to_string().contains(PKT12_SENTINEL));
        pkt12_assert_reaped(pid);
    }

    #[cfg(unix)]
    struct Pkt12SshFixture {
        script: std::path::PathBuf,
        transcript: std::path::PathBuf,
        expected_calls: usize,
    }

    #[cfg(unix)]
    impl Pkt12SshFixture {
        fn write(
            root: &std::path::Path,
            command: &str,
            malformed: &[u8],
            native_exit: bool,
        ) -> Self {
            use std::os::unix::fs::PermissionsExt;

            use crate::git_protocol::add_pkt_line_string;

            let successful_advertisements = if native_exit {
                0
            } else {
                match command {
                    "ls-remote" => 0,
                    "clone" => 2,
                    _ => 1,
                }
            };
            let mut valid = BytesMut::new();
            let oid = "1111111111111111111111111111111111111111";
            if command == "push" {
                add_pkt_line_string(
                    &mut valid,
                    format!(
                        "{oid} refs/heads/main\0report-status delete-refs object-format=sha1\n"
                    ),
                );
            } else {
                add_pkt_line_string(
                    &mut valid,
                    format!(
                        "{oid} HEAD\0multi_ack_detailed side-band-64k ofs-delta symref=HEAD:refs/heads/main object-format=sha1\n"
                    ),
                );
                add_pkt_line_string(&mut valid, format!("{oid} refs/heads/main\n"));
            }
            valid.extend_from_slice(b"0000");
            let encode = |wire: &[u8]| {
                wire.iter()
                    .map(|b| format!("\\{b:03o}"))
                    .collect::<String>()
            };
            let counter = root.join("ssh-count");
            let transcript = root.join("ssh-calls");
            let script = root.join("fake-ssh");
            std::fs::write(&counter, "0\n").unwrap();
            let counter_arg = shell_single_quote(counter.to_str().unwrap());
            let log_arg = shell_single_quote(transcript.to_str().unwrap());
            let stderr = if native_exit {
                format!("Permission denied (publickey). {PKT12_SENTINEL}")
            } else {
                PKT12_SENTINEL.to_string()
            };
            let termination = if native_exit {
                "exit 255"
            } else {
                "exec sleep 30"
            };
            let script_text = format!(
                "#!/bin/sh\nset -eu\nread -r count < {counter_arg}\ncount=$((count + 1))\nprintf '%s\\n' \"$count\" > {counter_arg}\nremote_command=''\nfor arg; do remote_command=$arg; done\nprintf '%s %s\\n' \"$$\" \"$remote_command\" >> {log_arg}\nif [ \"$count\" -le {successful_advertisements} ]; then\n  printf '{}'\n  exit 0\nfi\nprintf '%s' '{stderr}' >&2\nprintf '{}'\nexec 1>&-\n{termination}\n",
                encode(&valid),
                encode(malformed),
            );
            std::fs::write(&script, script_text).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            Self {
                script,
                transcript,
                expected_calls: successful_advertisements + 1,
            }
        }

        fn assert_calls_and_last_reaped(&self, command: &str) {
            let log = std::fs::read_to_string(&self.transcript).unwrap();
            let lines = log.lines().collect::<Vec<_>>();
            assert_eq!(lines.len(), self.expected_calls, "{command}: {log}");
            let expected_service = if command == "push" {
                "git-receive-pack"
            } else {
                "git-upload-pack"
            };
            for line in &lines {
                let (_, service) = line.split_once(' ').unwrap();
                assert_eq!(service, format!("{expected_service} 'repo'"));
            }
            let (pid, _) = lines.last().unwrap().split_once(' ').unwrap();
            pkt12_assert_reaped(pid.parse().unwrap());
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial(env, cwd, hash_kind)]
    async fn pkt_line_client_ssh_frame_errors_end_to_end_net_002() {
        use clap::Parser;

        use crate::{
            command::{clone, fetch, ls_remote, pull, push},
            git_protocol::PKT_LINE_PROTOCOL_ERROR_PREFIX,
            internal::{branch::Branch, config::ConfigKv},
            utils::{
                error::StableErrorCode,
                output::OutputConfig,
                test::{ChangeDirGuard, ScopedEnvVar, setup_with_new_libra_in},
            },
        };

        // The existing local fallback avoids cloud lookups for delete-only push.
        // Keyed lanes and per-test nextest processes follow existing env fixtures.
        let _storage = ScopedEnvVar::set("LIBRA_STORAGE_TYPE", "local");
        let mut checked = 0;
        for (malformed, native_exit) in [
            b"0001".as_slice(),
            b"0002",
            b"0003",
            b"",
            b"0",
            b"00",
            b"000",
            b"0005",
            b"0008abc",
            b"ffffabc",
        ]
        .into_iter()
        .map(|wire| (wire, false))
        .chain([(b"".as_slice(), true)])
        {
            let expected_reason = if matches!(malformed, b"0001" | b"0002" | b"0003") {
                PktLineError::InvalidFrameLength(
                    crate::git_protocol::PktFrameError::LengthBelowHeader,
                )
            } else if malformed.len() < 4 {
                PktLineError::TruncatedHeader
            } else {
                PktLineError::TruncatedPayload
            };
            for command in ["ls-remote", "fetch", "clone", "pull", "push"] {
                let repo = tempfile::tempdir().unwrap();
                setup_with_new_libra_in(repo.path()).await;
                let _cwd = ChangeDirGuard::new(repo.path());
                let fixture = Pkt12SshFixture::write(repo.path(), command, malformed, native_exit);
                let _ssh = ScopedEnvVar::set("LIBRA_SSH_COMMAND", &fixture.script);
                ConfigKv::set("remote.origin.url", "git@fixture.invalid:repo", false)
                    .await
                    .unwrap();
                let tracking = "refs/remotes/origin/main";
                let oid = "1111111111111111111111111111111111111111";
                if command == "push" {
                    Branch::update_branch(tracking, oid, Some("origin"))
                        .await
                        .unwrap();
                }
                let target = repo.path().join("clone-target");
                let output = OutputConfig::default();
                let error = tokio::time::timeout(Duration::from_secs(45), async {
                    match command {
                        "ls-remote" => ls_remote::execute_safe(
                            ls_remote::LsRemoteArgs::try_parse_from(["ls-remote", "origin"])
                                .unwrap(),
                            &output,
                        )
                        .await
                        .unwrap_err(),
                        "fetch" => fetch::execute_safe(
                            fetch::FetchArgs::try_parse_from(["fetch", "origin"]).unwrap(),
                            &output,
                        )
                        .await
                        .unwrap_err(),
                        "clone" => clone::execute_safe(
                            clone::CloneArgs::try_parse_from([
                                "clone",
                                "git@fixture.invalid:repo",
                                target.to_str().unwrap(),
                            ])
                            .unwrap(),
                            &output,
                        )
                        .await
                        .unwrap_err(),
                        "pull" => pull::execute_safe(
                            pull::PullArgs::try_parse_from(["pull", "--ff-only", "origin", "main"])
                                .unwrap(),
                            &output,
                        )
                        .await
                        .unwrap_err(),
                        "push" => push::execute_safe(
                            push::PushArgs::try_parse_from(["push", "origin", ":refs/heads/main"])
                                .unwrap(),
                            &output,
                        )
                        .await
                        .unwrap_err(),
                        _ => unreachable!(),
                    }
                })
                .await
                .expect("malformed SSH command must terminate");
                assert_eq!(
                    error.stable_code(),
                    StableErrorCode::NetworkProtocol,
                    "{command}: {error:?}"
                );
                assert_eq!(error.stable_code().as_str(), "LBR-NET-002");
                assert_eq!(error.stable_code().exit_code().as_i32(), 128);
                assert!(
                    error.message().contains(PKT_LINE_PROTOCOL_ERROR_PREFIX),
                    "{command}: {error:?}"
                );
                assert!(
                    error.message().contains(&expected_reason.to_string()),
                    "{command}: {error:?}"
                );
                if native_exit {
                    assert!(
                        error.message().contains("SSH exited with status 255"),
                        "{command}: {error:?}"
                    );
                    assert!(
                        error.message().contains("ssh-agent authentication"),
                        "{command}: {error:?}"
                    );
                    assert!(!error.message().contains("Permission denied (publickey)"));
                }
                let hint = if command == "push" {
                    "check the remote Git service or proxy response and retry"
                } else {
                    "check that the remote serves Git data and that a proxy has not altered the response"
                };
                assert_eq!(
                    error.hints().iter().map(|h| h.as_str()).collect::<Vec<_>>(),
                    [hint]
                );
                for rendered in [
                    error.render(),
                    error.render_report(),
                    error.render_json().to_string(),
                ] {
                    assert!(!rendered.contains(PKT12_SENTINEL), "{command}: {rendered}");
                }
                fixture.assert_calls_and_last_reaped(command);
                if command == "push" {
                    assert_eq!(
                        Branch::find_branch_result(tracking, Some("origin"))
                            .await
                            .unwrap()
                            .unwrap()
                            .commit
                            .to_string(),
                        oid
                    );
                }
                checked += 1;
            }
        }
        assert_eq!(checked, 55);
    }

    pub(crate) async fn read_test_stream<R: AsyncRead + Unpin>(
        stream: &mut R,
        idle: Duration,
    ) -> Result<Bytes, IoError> {
        let client = SshClient::from_ssh_spec("git@fixture.invalid:repo")
            .unwrap()
            .with_idle_timeout(idle);
        client.read_advertisement(stream, &mut false).await
    }

    pub(crate) async fn read_frame_fixture(mut input: &[u8]) -> Result<Bytes, IoError> {
        read_test_stream(&mut input, Duration::from_secs(1)).await
    }

    #[tokio::test]
    async fn pkt_line_client_ssh_rejects_len_below_four() {
        for input in [b"0001", b"0002", b"0003"] {
            crate::internal::protocol::git_client::tests::assert_typed_frame_error(
                read_frame_fixture(input).await.unwrap_err(),
                PktLineError::InvalidFrameLength(
                    crate::git_protocol::PktFrameError::LengthBelowHeader,
                ),
            );
        }
    }

    #[tokio::test]
    async fn pkt_line_client_ssh_flush_regression() {
        assert_eq!(
            read_frame_fixture(b"0000zzzz").await.unwrap(),
            b"0000".as_slice()
        );
    }

    #[tokio::test]
    async fn pkt_line_client_ssh_len4_regression() {
        let input = b"00040005x0000";
        assert_eq!(read_frame_fixture(input).await.unwrap(), input.as_slice());
    }

    #[tokio::test]
    async fn pkt_line_client_ssh_upper_bound_regression() {
        for header in [b"ffff", b"FFFF"] {
            let mut input = header.to_vec();
            input.extend_from_slice(&vec![0xff; 0xffff - 4]);
            input.extend_from_slice(b"0000");
            assert_eq!(read_frame_fixture(&input).await.unwrap(), input.as_slice());
        }
    }

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
        // The default defers host-key policy to ssh_config. BatchMode still
        // prevents interactive TOFU and passphrase prompts.
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
