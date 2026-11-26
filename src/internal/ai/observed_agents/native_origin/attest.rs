//! Bounded, domain-separated installation challenge. Successful attestation
//! proves key/binary association only; it creates no receipt or artifact.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};
#[cfg(unix)]
use std::{fs::OpenOptions, io::Read};

#[cfg(not(unix))]
use anyhow::bail;
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use ring::{
    rand::{SecureRandom, SystemRandom},
    signature,
};
#[cfg(feature = "storage-sqlite")]
use sea_orm::ConnectionTrait;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
};

use super::{
    approved_build, codec,
    registry::{IssuerRecord, IssuerState, NativeOriginError, decode_owned, lower_hex},
};
use crate::internal::ai::observed_agents::{
    rpc::RPC_MAX_STDERR_BYTES,
    trust::{self, TrustRecord},
};

const ATTEST_BYTE_CAP: usize = 4096;
const ATTEST_DEADLINE: Duration = Duration::from_secs(30);
const EXECUTABLE_BYTE_CAP: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Installation {
    version: u32,
    build_identity: String,
    producer_binary_digest: String,
    issuer_public_key: String,
    key_generation: u32,
    policy_descriptor: codec::Policy,
    policy_digest: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Attestation {
    version: u32,
    challenge: String,
    issuer: String,
    public_key: String,
    key_generation: u32,
    build_identity: String,
    producer_version: String,
    producer_binary_digest: String,
    policy_digest: String,
    signature: String,
}

pub(super) struct PreparedRegistration {
    pub(super) exporter_trust: TrustRecord,
    pub(super) record: IssuerRecord,
    // Transient actual executable measurement. It is never a signed policy or
    // persisted registry field and need not match a historical Libra build.
    pub(super) observer_executable_sha256: String,
}

struct LocalBinding {
    installation: Installation,
    producer_version: &'static str,
    key_dir: PathBuf,
    command_path: PathBuf,
    observer_executable_sha256: String,
}

/// No-follow, nonblocking open refuses symlinks and FIFOs before a read can
/// block. Compare identity and posture on the descriptor and pathname again.
/// This is the existing best-effort local-user tier, not an fd-exec guarantee.
#[cfg(unix)]
fn read_local_file(path: &Path, cap: usize, owned_mode: Option<u32>) -> Result<Vec<u8>> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let parent = path.parent().ok_or(NativeOriginError::Descriptor)?;
    if !path.is_absolute() || parent.canonicalize()? != parent {
        return Err(NativeOriginError::Descriptor.into());
    }
    let directory = fs::symlink_metadata(parent)?;
    // SAFETY: geteuid reads this process's uid and has no pointer arguments.
    let uid = unsafe { libc::geteuid() };
    if !directory.is_dir()
        || directory.uid() != uid
        || directory.permissions().mode() & 0o022 != 0
        || (owned_mode.is_some() && directory.permissions().mode() & 0o7777 != 0o700)
    {
        return Err(NativeOriginError::Descriptor.into());
    }
    let before = fs::symlink_metadata(path)?;
    let check = |meta: &fs::Metadata| {
        meta.is_file()
            && meta.uid() == uid
            && meta.len() <= cap as u64
            && meta.permissions().mode() & 0o022 == 0
            && owned_mode.is_none_or(|mode| meta.permissions().mode() & 0o7777 == mode)
    };
    if !check(&before) {
        return Err(NativeOriginError::Descriptor.into());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let opened = file.metadata()?;
    if !check(&opened) || opened.dev() != before.dev() || opened.ino() != before.ino() {
        return Err(NativeOriginError::Descriptor.into());
    }
    let mut bytes = Vec::new();
    (&mut file).take((cap + 1) as u64).read_to_end(&mut bytes)?;
    let after = fs::symlink_metadata(path)?;
    let final_meta = file.metadata()?;
    if bytes.len() > cap
        || bytes.len() as u64 != opened.len()
        || !check(&after)
        || final_meta.dev() != opened.dev()
        || final_meta.ino() != opened.ino()
        || final_meta.len() != opened.len()
        || final_meta.mtime() != opened.mtime()
        || final_meta.mtime_nsec() != opened.mtime_nsec()
        || after.dev() != opened.dev()
        || after.ino() != opened.ino()
        || after.len() != opened.len()
        || after.mtime() != opened.mtime()
        || after.mtime_nsec() != opened.mtime_nsec()
    {
        return Err(NativeOriginError::Descriptor.into());
    }
    Ok(bytes)
}

#[cfg(not(unix))]
fn read_local_file(_path: &Path, _cap: usize, _owned_mode: Option<u32>) -> Result<Vec<u8>> {
    bail!("Native-origin registration requires the reviewed macOS private producer installation")
}

fn measure_executable(path: &Path) -> Result<trust::Provenance> {
    let canonical = path
        .canonicalize()
        .context("resolve controlled native-origin executable")?;
    let meta = fs::symlink_metadata(&canonical)?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > EXECUTABLE_BYTE_CAP {
        return Err(NativeOriginError::PriorTrust.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // SAFETY: geteuid reads this process's uid and has no pointer arguments.
        let uid = unsafe { libc::geteuid() };
        if (meta.uid() != uid && meta.uid() != 0)
            || meta.permissions().mode() & 0o022 != 0
            || meta.permissions().mode() & 0o111 == 0
        {
            return Err(NativeOriginError::PriorTrust.into());
        }
    }
    trust::ensure_parent_not_world_writable(&canonical)?;
    trust::compute_provenance(&canonical)
}

fn local_binding(worktree: &Path, manifest: &Path, trusted: &TrustRecord) -> Result<LocalBinding> {
    if manifest
        .file_name()
        .is_none_or(|name| name != "installation.json")
    {
        return Err(NativeOriginError::Descriptor.into());
    }
    let raw = read_local_file(manifest, codec::DESCRIPTOR_BYTE_CAP, Some(0o600))
        .context("read owned public installation.json; Libra never reads the private key")?;
    let installation: Installation = decode_owned(&raw)?;
    lower_hex(&installation.build_identity, 16)?;
    lower_hex(&installation.producer_binary_digest, 32)?;
    lower_hex(&installation.issuer_public_key, 32)?;
    lower_hex(&installation.policy_digest, 32)?;
    if installation.version != 1 || installation.key_generation == 0 {
        return Err(NativeOriginError::Descriptor.into());
    }
    let provenance = measure_executable(&trusted.path)?;
    if trust::provenance_drifted(&provenance, trusted)
        || provenance.sha256 != installation.producer_binary_digest
    {
        return Err(NativeOriginError::PriorTrust.into());
    }
    // Select the catalog BEFORE passing a private installation directory to a
    // child. A trusted ordinary exporter is not an approved attest producer.
    let build = approved_build::select_measured(&installation.build_identity, &provenance.sha256)?;
    let command = measure_executable(
        &std::env::current_exe()
            .context("locate current Libra executable for native-origin registration")?,
    )?;
    let command_path = command.canonical_path;
    let command_text = command_path.to_str().ok_or(NativeOriginError::Observer)?;
    let observer = read_local_file(
        &worktree.join(".opencode/plugin/libra-hooks.js"),
        codec::DESCRIPTOR_BYTE_CAP,
        None,
    )
    .map_err(|_| NativeOriginError::Observer)?;
    let policy = build.installation_policy(command_text, &observer)?;
    let policy_digest = hex::encode(Sha256::digest(codec::policy_bytes(&policy)?));
    if installation.policy_descriptor != policy || installation.policy_digest != policy_digest {
        return Err(NativeOriginError::Observer.into());
    }
    Ok(LocalBinding {
        installation,
        producer_version: build.identity().producer_version,
        key_dir: manifest
            .parent()
            .ok_or(NativeOriginError::Descriptor)?
            .to_owned(),
        command_path,
        observer_executable_sha256: command.sha256,
    })
}

async fn bounded_read<R: AsyncRead + Unpin>(mut reader: R, cap: usize) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = reader
            .read(&mut chunk)
            .await
            .context("read native-origin child pipe")?;
        if n == 0 {
            return Ok(result);
        }
        if result.len() + n > cap {
            return Err(NativeOriginError::ChildBounds.into());
        }
        result.extend_from_slice(&chunk[..n]);
    }
}

const LAUNCH_DIRECTORIES: [&str; 6] = ["home", "tmp", "config", "data", "cache", "state"];

struct AttestLaunch {
    directory: TempDir,
    canonical: PathBuf,
    #[cfg(test)]
    fail_cleanup: bool,
}

impl AttestLaunch {
    fn new(key_dir: &Path) -> Result<Self> {
        Self::new_inner(
            key_dir,
            #[cfg(test)]
            None,
        )
    }

    fn new_inner(key_dir: &Path, #[cfg(test)] fail_after_directory: Option<usize>) -> Result<Self> {
        #[cfg(not(unix))]
        {
            let _ = key_dir;
            #[cfg(test)]
            let _ = fail_after_directory;
            Err(NativeOriginError::Challenge.into())
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

            let metadata =
                fs::symlink_metadata(key_dir).map_err(|_| NativeOriginError::Challenge)?;
            if metadata.file_type().is_symlink() {
                return Err(NativeOriginError::Challenge.into());
            }
            let parent = key_dir
                .canonicalize()
                .map_err(|_| NativeOriginError::Challenge)?;
            Self::validate_directory(&parent, false)?;
            let directory = tempfile::Builder::new()
                .prefix(".libra-native-attest-")
                .permissions(fs::Permissions::from_mode(0o700))
                .tempdir_in(parent)
                .map_err(|_| NativeOriginError::Challenge)?;
            let canonical = directory
                .path()
                .canonicalize()
                .map_err(|_| NativeOriginError::Challenge)?;
            for name in LAUNCH_DIRECTORIES {
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(canonical.join(name))
                    .map_err(|_| NativeOriginError::Challenge)?;
                #[cfg(test)]
                if fail_after_directory
                    == Some(
                        LAUNCH_DIRECTORIES
                            .iter()
                            .position(|candidate| *candidate == name)
                            .ok_or(NativeOriginError::Challenge)?,
                    )
                {
                    return Err(NativeOriginError::Challenge.into());
                }
            }
            let launch = Self {
                directory,
                canonical,
                #[cfg(test)]
                fail_cleanup: false,
            };
            launch.validate()?;
            Ok(launch)
        }
    }

    fn validate_directory(path: &Path, empty: bool) -> Result<()> {
        #[cfg(not(unix))]
        {
            let _ = (path, empty);
            Err(NativeOriginError::Challenge.into())
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};

            let metadata = fs::symlink_metadata(path).map_err(|_| NativeOriginError::Challenge)?;
            // SAFETY: geteuid has no pointer arguments and reads this process's uid.
            let uid = unsafe { libc::geteuid() };
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || metadata.uid() != uid
                || metadata.permissions().mode() & 0o7777 != 0o700
                || path
                    .canonicalize()
                    .map_err(|_| NativeOriginError::Challenge)?
                    != path
            {
                return Err(NativeOriginError::Challenge.into());
            }
            if empty
                && fs::read_dir(path)
                    .map_err(|_| NativeOriginError::Challenge)?
                    .next()
                    .transpose()
                    .map_err(|_| NativeOriginError::Challenge)?
                    .is_some()
            {
                return Err(NativeOriginError::Challenge.into());
            }
            Ok(())
        }
    }

    fn validate(&self) -> Result<()> {
        Self::validate_directory(&self.canonical, false)?;
        let mut count = 0;
        for entry in fs::read_dir(&self.canonical)
            .map_err(|_| NativeOriginError::Challenge)?
            .take(LAUNCH_DIRECTORIES.len() + 1)
        {
            let entry = entry.map_err(|_| NativeOriginError::Challenge)?;
            if !LAUNCH_DIRECTORIES
                .iter()
                .any(|name| entry.file_name() == *name)
            {
                return Err(NativeOriginError::Challenge.into());
            }
            count += 1;
        }
        if count != LAUNCH_DIRECTORIES.len() {
            return Err(NativeOriginError::Challenge.into());
        }
        for name in LAUNCH_DIRECTORIES {
            Self::validate_directory(&self.canonical.join(name), true)?;
        }
        Ok(())
    }

    fn environment(&self, key_dir: &Path, command: &Path) -> [(&'static str, OsString); 15] {
        [
            ("PATH", OsString::from("/usr/bin:/bin:/usr/sbin:/sbin")),
            ("HOME", self.canonical.join("home").into_os_string()),
            ("TMPDIR", self.canonical.join("tmp").into_os_string()),
            (
                "XDG_CONFIG_HOME",
                self.canonical.join("config").into_os_string(),
            ),
            (
                "XDG_DATA_HOME",
                self.canonical.join("data").into_os_string(),
            ),
            (
                "XDG_CACHE_HOME",
                self.canonical.join("cache").into_os_string(),
            ),
            (
                "XDG_STATE_HOME",
                self.canonical.join("state").into_os_string(),
            ),
            ("LANG", OsString::from("C.UTF-8")),
            ("LC_ALL", OsString::from("C.UTF-8")),
            ("TERM", OsString::from("dumb")),
            ("NO_COLOR", OsString::from("1")),
            ("DO_NOT_TRACK", OsString::from("1")),
            ("OPENCODE_LIBRA_NATIVE_ORIGIN", OsString::from("1")),
            (
                "OPENCODE_LIBRA_NATIVE_KEY_DIR",
                key_dir.as_os_str().to_owned(),
            ),
            ("OPENCODE_LIBRA_COMMAND", command.as_os_str().to_owned()),
        ]
    }
}

async fn close_launch(launch: AttestLaunch) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        let result = launch.directory.close();
        #[cfg(test)]
        if launch.fail_cleanup {
            return Err(std::io::Error::other("synthetic cleanup failure"));
        }
        result
    })
    .await
    .map_err(|_| NativeOriginError::Challenge)?
    .map_err(|_| NativeOriginError::Challenge)?;
    Ok(())
}

struct AttestChild {
    child: Child,
    process_group: Option<i32>,
    launch: Option<AttestLaunch>,
}

impl AttestChild {
    fn terminate(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.process_group.take() {
            // SAFETY: this positive pid is the newly spawned child's private
            // process group (process_group(0)); no caller-supplied pid is used.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
            let _ = self.child.start_kill();
        }
        #[cfg(not(unix))]
        let _ = self.child.start_kill();
    }

    async fn wait_owned(&mut self) -> Result<ExitStatus> {
        #[cfg(unix)]
        {
            let pid = self.process_group.ok_or(NativeOriginError::Challenge)?;
            loop {
                let (result, exited) = {
                    // SAFETY: siginfo_t is a plain C output record; zero marks
                    // WNOHANG. Keep its non-Send pointers outside every await.
                    let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
                    // SAFETY: pid belongs to this unreaped child, and the output
                    // pointer is valid. WNOHANG prevents blocking this thread.
                    let result = unsafe {
                        libc::waitid(
                            libc::P_PID,
                            pid as libc::id_t,
                            &mut information,
                            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                        )
                    };
                    (result, information.si_signo != 0)
                };
                if result < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() == Some(libc::EINTR) {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        continue;
                    }
                    if error.raw_os_error() == Some(libc::ECHILD) {
                        self.process_group = None;
                    }
                    return Err(NativeOriginError::Challenge.into());
                }
                if exited {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            // Kill inherited descendants while the unreaped leader still
            // reserves the PGID, including on a successful challenge.
            self.terminate();
        }
        let status = self
            .child
            .wait()
            .await
            .context("wait for native-origin challenge")?;
        self.process_group = None;
        Ok(status)
    }

    async fn close_launch(&mut self) -> Result<()> {
        close_launch(self.launch.take().ok_or(NativeOriginError::Challenge)?).await
    }

    async fn terminate_and_reap(&mut self) -> Result<()> {
        self.terminate();
        tokio::time::timeout(Duration::from_secs(1), self.child.wait())
            .await
            .context("native-origin child did not reap after termination")?
            .context("reap terminated native-origin child")?;
        self.process_group = None;
        Ok(())
    }
}

impl Drop for AttestChild {
    fn drop(&mut self) {
        if self.process_group.is_some() {
            self.terminate();
        }
        // Tokio's kill_on_drop additionally covers cancellation on platforms
        // without process groups, and its child owner reaps a dropped child.
        if let Some(launch) = self.launch.take() {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn_blocking(move || {
                    let _ = launch.directory.close();
                });
            } else {
                drop(launch);
            }
        }
    }
}

async fn run_child(
    binary: &Path,
    worktree: &Path,
    key_dir: &Path,
    command_path: &Path,
    challenge: &str,
    deadline: Duration,
) -> Result<Vec<u8>> {
    let parent = key_dir.to_owned();
    let launch = tokio::task::spawn_blocking(move || AttestLaunch::new(&parent))
        .await
        .map_err(|_| NativeOriginError::Challenge)??;
    run_prepared_child(
        binary,
        worktree,
        key_dir,
        command_path,
        challenge,
        deadline,
        launch,
    )
    .await
}

async fn run_prepared_child(
    binary: &Path,
    worktree: &Path,
    key_dir: &Path,
    command_path: &Path,
    challenge: &str,
    deadline: Duration,
    launch: AttestLaunch,
) -> Result<Vec<u8>> {
    let launch = tokio::task::spawn_blocking(move || -> Result<AttestLaunch> {
        launch.validate()?;
        Ok(launch)
    })
    .await
    .map_err(|_| NativeOriginError::Challenge)??;
    let mut command = Command::new(binary);
    command
        .args(["native-origin", "attest", "--challenge", challenge])
        .current_dir(worktree)
        .env_clear()
        .envs(launch.environment(key_dir, command_path))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            let _ = close_launch(launch).await;
            return Err(NativeOriginError::Challenge.into());
        }
    };
    let group = child.id().and_then(|pid| i32::try_from(pid).ok());
    let mut owner = AttestChild {
        child,
        process_group: group,
        launch: Some(launch),
    };
    let stdout = owner
        .child
        .stdout
        .take()
        .ok_or(NativeOriginError::Challenge)?;
    let stderr = owner
        .child
        .stderr
        .take()
        .ok_or(NativeOriginError::Challenge)?;
    let result = tokio::time::timeout(deadline, async {
        let (stdout, _stderr, status) = tokio::try_join!(
            bounded_read(stdout, ATTEST_BYTE_CAP),
            bounded_read(stderr, RPC_MAX_STDERR_BYTES),
            owner.wait_owned(),
        )?;
        if !status.success() {
            return Err(NativeOriginError::Challenge.into());
        }
        Ok(stdout)
    })
    .await;
    let outcome = match result {
        Ok(Ok(bytes)) => Ok(bytes),
        other => {
            let error = match other {
                Ok(Err(error)) => error,
                Err(_) => NativeOriginError::ChildBounds.into(),
                Ok(Ok(_)) => return Err(NativeOriginError::Challenge.into()),
            };
            if owner.terminate_and_reap().await.is_err() {
                owner.close_launch().await?;
                return Err(NativeOriginError::Challenge.into());
            }
            // Captured stderr never reaches output, even on failure. The
            // fixed actionable diagnostic is the only surfaced child text.
            Err(error)
        }
    };
    owner.close_launch().await?;
    outcome
}

fn verify_challenge(bytes: &[u8], challenge: &str, local: &LocalBinding) -> Result<IssuerRecord> {
    if bytes.len() > ATTEST_BYTE_CAP || bytes.last() != Some(&b'\n') {
        return Err(NativeOriginError::Challenge.into());
    }
    let response: Attestation =
        decode_owned(&bytes[..bytes.len() - 1]).map_err(|_| NativeOriginError::Challenge)?;
    let installation = &local.installation;
    let public_key = lower_hex(&response.public_key, 32)?;
    let issuer = hex::encode(Sha256::digest(&public_key));
    if response.version != 1
        || response.challenge != challenge
        || response.issuer != issuer
        || response.public_key != installation.issuer_public_key
        || response.key_generation != installation.key_generation
        || response.build_identity != installation.build_identity
        || response.producer_version != local.producer_version
        || response.producer_binary_digest != installation.producer_binary_digest
        || response.policy_digest != installation.policy_digest
    {
        return Err(NativeOriginError::Challenge.into());
    }
    approved_build::lookup(
        &response.producer_version,
        &response.build_identity,
        &response.producer_binary_digest,
    )?;
    let signature = STANDARD
        .decode(&response.signature)
        .map_err(|_| NativeOriginError::Challenge)?;
    if signature.len() != 64 || STANDARD.encode(&signature) != response.signature {
        return Err(NativeOriginError::Challenge.into());
    }
    let mut preimage = b"libra-opencode-native-origin-attest-v1\0".to_vec();
    preimage.extend_from_slice(&response.version.to_be_bytes());
    preimage.extend(lower_hex(challenge, 32)?);
    preimage.extend(lower_hex(&issuer, 32)?);
    preimage.extend(&public_key);
    preimage.extend_from_slice(&response.key_generation.to_be_bytes());
    preimage.extend(lower_hex(&response.build_identity, 16)?);
    let version_len =
        u32::try_from(response.producer_version.len()).map_err(|_| NativeOriginError::Challenge)?;
    preimage.extend_from_slice(&version_len.to_be_bytes());
    preimage.extend_from_slice(response.producer_version.as_bytes());
    preimage.extend(lower_hex(&response.producer_binary_digest, 32)?);
    preimage.extend(lower_hex(&response.policy_digest, 32)?);
    signature::UnparsedPublicKey::new(&signature::ED25519, &public_key)
        .verify(&preimage, &signature)
        .map_err(|_| NativeOriginError::Challenge)?;
    Ok(IssuerRecord {
        issuer,
        public_key: response.public_key,
        key_generation: response.key_generation,
        build_identity: response.build_identity,
        producer_version: response.producer_version,
        producer_binary_digest: response.producer_binary_digest,
        producer_path: String::new(), // Set only from the actual trust owner below.
        policy_descriptor: installation.policy_descriptor.clone(),
        policy_digest: response.policy_digest,
        state: IssuerState::Active,
    })
}

async fn prepare_from_trust(
    exporter_trust: TrustRecord,
    dirs: Vec<PathBuf>,
    worktree: &Path,
    manifest: &Path,
) -> Result<PreparedRegistration> {
    if !trust::path_within_trusted_dirs(&exporter_trust.path, &dirs) {
        return Err(NativeOriginError::PriorTrust.into());
    }
    let repo = worktree.to_owned();
    let descriptor_path = manifest.to_owned();
    let trusted = exporter_trust.clone();
    let local =
        tokio::task::spawn_blocking(move || local_binding(&repo, &descriptor_path, &trusted))
            .await
            .context("measure native-origin installation binding")??;
    let mut nonce = [0u8; 32];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| NativeOriginError::Challenge)?;
    let challenge = hex::encode(nonce);
    let bytes = run_child(
        &exporter_trust.path,
        worktree,
        &local.key_dir,
        &local.command_path,
        &challenge,
        ATTEST_DEADLINE,
    )
    .await?;
    let mut record = verify_challenge(&bytes, &challenge, &local)?;
    let trusted = exporter_trust.clone();
    let unchanged = tokio::task::spawn_blocking(move || -> Result<()> {
        let measured = measure_executable(&trusted.path)?;
        if trust::provenance_drifted(&measured, &trusted) {
            return Err(NativeOriginError::PriorTrust.into());
        }
        Ok(())
    })
    .await
    .context("recheck exporter after native-origin challenge")?;
    unchanged?;
    record.producer_path = exporter_trust
        .path
        .to_str()
        .ok_or(NativeOriginError::PriorTrust)?
        .to_owned();
    Ok(PreparedRegistration {
        exporter_trust,
        record,
        observer_executable_sha256: local.observer_executable_sha256,
    })
}

#[cfg(feature = "storage-sqlite")]
pub(super) async fn prepare<C: ConnectionTrait>(
    conn: &C,
    worktree: &Path,
    manifest: &Path,
) -> Result<PreparedRegistration> {
    let exporter_trust = trust::read_trust_with_conn(conn, trust::OPENCODE_EXPORTER_SLUG)
        .await?
        .ok_or(NativeOriginError::PriorTrust)?;
    let dirs = trust::read_trusted_dirs_with_conn(conn).await?;
    prepare_from_trust(exporter_trust, dirs, worktree, manifest).await
}

/// Backend-agnostic counterpart of [`prepare`] using [`crate::storage::ConfigStore`].
pub(super) async fn prepare_with_store(
    store: &dyn crate::storage::ConfigStore,
    worktree: &Path,
    manifest: &Path,
) -> Result<PreparedRegistration> {
    let exporter_trust = trust::read_trust_with_store(store, trust::OPENCODE_EXPORTER_SLUG)
        .await?
        .ok_or(NativeOriginError::PriorTrust)?;
    let dirs = trust::read_trusted_dirs_with_store(store).await?;
    prepare_from_trust(exporter_trust, dirs, worktree, manifest).await
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use ring::signature::KeyPair;
    use serde_json::{Value, json};

    use super::*;

    fn script(root: &Path, text: &str) -> PathBuf {
        fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("synthetic-attest-child.sh");
        fs::write(&path, format!("#!/bin/sh\n{text}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn launch_directories(root: &Path) -> Vec<PathBuf> {
        fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".libra-native-attest-")
            })
            .collect()
    }

    #[tokio::test]
    async fn native_attest_private_launch_environment() {
        use std::{collections::BTreeMap, os::unix::fs::MetadataExt};

        let root = tempfile::tempdir().unwrap();
        let stat = if cfg!(target_os = "linux") {
            "/usr/bin/stat -c '%a:%u'"
        } else {
            "/usr/bin/stat -f '%Lp:%u'"
        };
        let child = script(
            root.path(),
            &format!(
                r#"expected_uid=$(/usr/bin/id -u); for directory in "$HOME" "$TMPDIR" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME" "$XDG_STATE_HOME"; do [ -d "$directory" ] && [ ! -L "$directory" ] && [ -z "$(/bin/ls -A "$directory")" ] && [ "$(cd "$directory" && pwd -P)" = "$directory" ] && [ "$({stat} "$directory")" = "700:$expected_uid" ] || exit 23; printf 'checked=%s\n' "$directory"; done; exec /usr/bin/env"#
            ),
        );
        let prepared = AttestLaunch::new(root.path()).unwrap();
        let expected_names = [
            "PATH",
            "HOME",
            "TMPDIR",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "XDG_STATE_HOME",
            "LANG",
            "LC_ALL",
            "TERM",
            "NO_COLOR",
            "DO_NOT_TRACK",
            "OPENCODE_LIBRA_NATIVE_ORIGIN",
            "OPENCODE_LIBRA_NATIVE_KEY_DIR",
            "OPENCODE_LIBRA_COMMAND",
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
        let actual = prepared.environment(root.path(), Path::new("/bin/sh"));
        assert_eq!(
            actual
                .iter()
                .map(|(name, _)| *name)
                .collect::<std::collections::BTreeSet<_>>(),
            expected_names
        );
        for name in ["home", "tmp", "config", "data", "cache", "state"] {
            let path = prepared.canonical.join(name);
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(metadata.is_dir());
            assert!(!metadata.file_type().is_symlink());
            assert_eq!(metadata.permissions().mode() & 0o7777, 0o700);
            // SAFETY: geteuid has no arguments and reads this test process's uid.
            assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
            assert_eq!(path.canonicalize().unwrap(), path);
            assert_eq!(fs::read_dir(path).unwrap().count(), 0);
        }
        close_launch(prepared).await.unwrap();
        let mut prior_tmp = None;
        for _ in 0..2 {
            let bytes = run_child(
                &child,
                root.path(),
                root.path(),
                Path::new("/bin/sh"),
                &"0".repeat(64),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
            let text = String::from_utf8(bytes).unwrap();
            let mut lines = text.lines();
            let checked = lines
                .by_ref()
                .take(6)
                .map(|line| PathBuf::from(line.strip_prefix("checked=").unwrap()))
                .collect::<Vec<_>>();
            assert_eq!(
                checked.len(),
                6,
                "the child must verify all six directories"
            );
            let environment = lines
                .map(|line| line.split_once('=').unwrap())
                .filter(|(name, _)| !["PWD", "SHLVL", "_"].contains(name))
                .collect::<BTreeMap<_, _>>();
            assert_eq!(
                environment
                    .keys()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>(),
                expected_names
            );
            assert_eq!(environment["PATH"], "/usr/bin:/bin:/usr/sbin:/sbin");
            assert_eq!(environment["LANG"], "C.UTF-8");
            assert_eq!(environment["LC_ALL"], "C.UTF-8");
            assert_eq!(environment["TERM"], "dumb");
            assert_eq!(environment["NO_COLOR"], "1");
            assert_eq!(environment["DO_NOT_TRACK"], "1");
            assert_eq!(environment["OPENCODE_LIBRA_NATIVE_ORIGIN"], "1");
            assert_eq!(
                environment["OPENCODE_LIBRA_NATIVE_KEY_DIR"],
                root.path().to_str().unwrap()
            );
            assert_eq!(environment["OPENCODE_LIBRA_COMMAND"], "/bin/sh");
            let tmp = PathBuf::from(environment["TMPDIR"]);
            assert!(tmp.starts_with(root.path().canonicalize().unwrap()));
            let scratch = tmp.parent().unwrap();
            for (key, directory) in [
                ("HOME", "home"),
                ("TMPDIR", "tmp"),
                ("XDG_CONFIG_HOME", "config"),
                ("XDG_DATA_HOME", "data"),
                ("XDG_CACHE_HOME", "cache"),
                ("XDG_STATE_HOME", "state"),
            ] {
                assert_eq!(Path::new(environment[key]), scratch.join(directory));
                assert!(checked.contains(&scratch.join(directory)));
            }
            assert!(
                !tmp.exists(),
                "completed challenge must remove its private TMPDIR"
            );
            assert_ne!(prior_tmp.as_ref(), Some(&tmp));
            prior_tmp = Some(tmp);
        }
        assert!(launch_directories(root.path()).is_empty());
    }

    #[tokio::test]
    async fn native_attest_rejects_unsafe_launch_before_spawn() {
        let root = tempfile::tempdir().unwrap();
        let child = script(root.path(), "echo executed > ./executed");
        let challenge = "0".repeat(64);
        let not_directory = root.path().join("not-a-directory");
        fs::write(&not_directory, b"not a directory").unwrap();
        assert_eq!(
            run_child(
                &child,
                root.path(),
                &not_directory,
                Path::new("/bin/sh"),
                &challenge,
                Duration::from_secs(2)
            )
            .await
            .unwrap_err()
            .downcast_ref(),
            Some(&NativeOriginError::Challenge)
        );
        let alias = root.path().join("key-directory-alias");
        std::os::unix::fs::symlink(root.path(), &alias).unwrap();
        assert_eq!(
            run_child(
                &child,
                root.path(),
                &alias,
                Path::new("/bin/sh"),
                &challenge,
                Duration::from_secs(2)
            )
            .await
            .unwrap_err()
            .downcast_ref(),
            Some(&NativeOriginError::Challenge)
        );
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            run_child(
                &child,
                root.path(),
                root.path(),
                Path::new("/bin/sh"),
                &challenge,
                Duration::from_secs(2)
            )
            .await
            .unwrap_err()
            .downcast_ref(),
            Some(&NativeOriginError::Challenge)
        );
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        for case in 0..3 {
            let launch = AttestLaunch::new(root.path()).unwrap();
            let path = launch.canonical.clone();
            let tmp = path.join("tmp");
            match case {
                0 => fs::write(tmp.join(".bun-planted"), b"wrong native library").unwrap(),
                1 => fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).unwrap(),
                _ => {
                    fs::remove_dir(&tmp).unwrap();
                    std::os::unix::fs::symlink(root.path(), &tmp).unwrap();
                }
            }
            assert_eq!(
                run_prepared_child(
                    &child,
                    root.path(),
                    root.path(),
                    Path::new("/bin/sh"),
                    &challenge,
                    Duration::from_secs(2),
                    launch
                )
                .await
                .unwrap_err()
                .downcast_ref(),
                Some(&NativeOriginError::Challenge)
            );
            assert!(!path.exists());
            assert!(!root.path().join("executed").exists());
        }
        assert!(!root.path().join("executed").exists());
    }

    #[tokio::test]
    async fn native_attest_launch_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let challenge = "0".repeat(64);
        for (body, expected, budget) in [
            ("printf '{}\\n'", None, Duration::from_secs(2)),
            (
                "exit 7",
                Some(NativeOriginError::Challenge),
                Duration::from_secs(2),
            ),
            (
                "exec /usr/bin/head -c 4097 /dev/zero",
                Some(NativeOriginError::ChildBounds),
                Duration::from_secs(2),
            ),
            (
                "exec /bin/sleep 10",
                Some(NativeOriginError::ChildBounds),
                Duration::from_millis(150),
            ),
        ] {
            let child = script(root.path(), body);
            let result = run_child(
                &child,
                root.path(),
                root.path(),
                Path::new("/bin/sh"),
                &challenge,
                budget,
            )
            .await;
            if let Some(expected) = expected {
                assert_eq!(result.unwrap_err().downcast_ref(), Some(&expected));
            } else {
                assert_eq!(result.unwrap(), b"{}\n");
            }
            assert!(
                launch_directories(root.path()).is_empty(),
                "every completed failure/success path must remove scratch"
            );
        }
        let missing = root.path().join("missing-executable");
        assert_eq!(
            run_child(
                &missing,
                root.path(),
                root.path(),
                Path::new("/bin/sh"),
                &challenge,
                Duration::from_secs(2)
            )
            .await
            .unwrap_err()
            .downcast_ref(),
            Some(&NativeOriginError::Challenge)
        );
        assert!(launch_directories(root.path()).is_empty());
        for count in 0..6 {
            assert!(AttestLaunch::new_inner(root.path(), Some(count)).is_err());
            assert!(launch_directories(root.path()).is_empty());
        }
        let child = script(root.path(), "printf '{}\\n'");
        let mut launch = AttestLaunch::new(root.path()).unwrap();
        launch.fail_cleanup = true;
        assert_eq!(
            run_prepared_child(
                &child,
                root.path(),
                root.path(),
                Path::new("/bin/sh"),
                &challenge,
                Duration::from_secs(2),
                launch
            )
            .await
            .unwrap_err()
            .downcast_ref(),
            Some(&NativeOriginError::Challenge)
        );
        assert!(launch_directories(root.path()).is_empty());
        let child = script(
            root.path(),
            "echo \"$TMPDIR\" > \"$OPENCODE_LIBRA_NATIVE_KEY_DIR/observed-tmp\"; exec /bin/sleep 10",
        );
        let path = child.clone();
        let cwd = root.path().to_owned();
        let task = tokio::spawn(async move {
            run_child(
                &path,
                &cwd,
                &cwd,
                Path::new("/bin/sh"),
                &"0".repeat(64),
                Duration::from_secs(30),
            )
            .await
        });
        let observed = root.path().join("observed-tmp");
        tokio::time::timeout(Duration::from_secs(3), async {
            while !observed.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let tmp = PathBuf::from(fs::read_to_string(observed).unwrap().trim());
        assert!(tmp.is_dir(), "scratch must live while child is active");
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(3), async {
            while tmp.exists() || !launch_directories(root.path()).is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        let child = script(
            root.path(),
            "(/bin/sleep 10 </dev/null >/dev/null 2>&1 & echo $! > \"$OPENCODE_LIBRA_NATIVE_KEY_DIR/descendant-pid\"); printf '{}\\n'",
        );
        assert_eq!(
            run_child(
                &child,
                root.path(),
                root.path(),
                Path::new("/bin/sh"),
                &challenge,
                Duration::from_secs(2)
            )
            .await
            .unwrap(),
            b"{}\n"
        );
        let pid: i32 = fs::read_to_string(root.path().join("descendant-pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                // SAFETY: signal 0 probes only this fixture's recorded descendant.
                if unsafe { libc::kill(pid, 0) } != 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("successful challenge must terminate its inherited group");
        assert!(launch_directories(root.path()).is_empty());
    }

    // These fixtures exercise the private runner's mechanics only. Their
    // hashes are never catalog entries, and no fixture can register an issuer.
    #[tokio::test]
    async fn npr_09_child_mechanics() {
        let root = tempfile::tempdir().unwrap();
        let path = script(root.path(), "exec /usr/bin/head -c 4096 /dev/zero");
        let challenge = "0".repeat(64);
        let run = |duration| {
            run_child(
                &path,
                root.path(),
                root.path(),
                Path::new("/bin/sh"),
                &challenge,
                duration,
            )
        };
        assert_eq!(run(Duration::from_secs(2)).await.unwrap().len(), 4096);
        script(root.path(), "exec /usr/bin/head -c 4097 /dev/zero");
        let error = run(Duration::from_secs(2)).await.unwrap_err();
        assert_eq!(error.downcast_ref(), Some(&NativeOriginError::ChildBounds));
        script(root.path(), "printf '{}\\n'; exit 7");
        assert_eq!(
            run(Duration::from_secs(2))
                .await
                .unwrap_err()
                .downcast_ref(),
            Some(&NativeOriginError::Challenge)
        );
        script(
            root.path(),
            "printf 'private-child-secret\\033[31m' >&2; exit 1",
        );
        let error = run(Duration::from_secs(2)).await.unwrap_err().to_string();
        assert!(!error.contains("private-child-secret"));
        assert!(!error.contains('\u{1b}'));
        script(root.path(), "exec /usr/bin/head -c 65537 /dev/zero >&2");
        assert_eq!(
            run(Duration::from_secs(2))
                .await
                .unwrap_err()
                .downcast_ref(),
            Some(&NativeOriginError::ChildBounds)
        );
        for body in [
            "exec /bin/sleep 10",
            "printf '{}\\n'; exec /bin/sleep 10",
            "while :; do printf x; /bin/sleep 0.05; done",
        ] {
            script(root.path(), body);
            let start = std::time::Instant::now();
            assert_eq!(
                run(Duration::from_millis(150))
                    .await
                    .unwrap_err()
                    .downcast_ref(),
                Some(&NativeOriginError::ChildBounds)
            );
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "timeout must include cleanup"
            );
        }
        script(
            root.path(),
            "test -z \"$NODE_OPTIONS$BUN_OPTIONS$LIBRA_D1_TOKEN$OPENCODE_CONFIG\" || exit 9; test \"$OPENCODE_LIBRA_NATIVE_ORIGIN\" = 1 || exit 8; printf '{}\\n'",
        );
        assert_eq!(run(Duration::from_secs(2)).await.unwrap(), b"{}\n");
    }

    #[tokio::test]
    async fn npr_09_cancel_reaps_child() {
        let root = tempfile::tempdir().unwrap();
        let path = script(
            root.path(),
            "echo $$ > \"$OPENCODE_LIBRA_NATIVE_KEY_DIR/pid\"; exec /bin/sleep 10",
        );
        let cwd = root.path().to_owned();
        let task = tokio::spawn(async move {
            run_child(
                &path,
                &cwd,
                &cwd,
                Path::new("/bin/sh"),
                &"0".repeat(64),
                Duration::from_secs(30),
            )
            .await
        });
        let pid_file = root.path().join("pid");
        let pid: i32 = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(text) = fs::read_to_string(&pid_file)
                    && let Ok(pid) = text.trim().parse()
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                // SAFETY: signal 0 only probes the specific fixture pid.
                if unsafe { libc::kill(pid, 0) } != 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cancelled fixture child must be reaped");
    }

    #[test]
    fn npr_09_owned_file_posture() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let root = root.path().canonicalize().unwrap();
        let file = root.join("installation.json");
        fs::write(&file, b"{}").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_local_file(&file, 2, Some(0o600)).unwrap(), b"{}");
        assert!(read_local_file(&file, 1, Some(0o600)).is_err());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_local_file(&file, 2, Some(0o600)).is_err());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(read_local_file(&file, 2, Some(0o600)).is_err());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let link = root.join("alias.json");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(read_local_file(&link, 2, Some(0o600)).is_err());
    }

    #[test]
    fn npr_09_challenge_bindings() {
        // Synthetic public-wire predicate tests, not approved CLI enrollment
        // or native provider evidence. The real CLI tests separately consume
        // an independently implemented producer signature from actual bytes.
        let key = signature::Ed25519KeyPair::from_seed_unchecked(&[5u8; 32]).unwrap();
        let public = hex::encode(key.public_key().as_ref());
        let issuer = hex::encode(Sha256::digest(key.public_key().as_ref()));
        let challenge = "1".repeat(64);
        let build = approved_build::lookup(
            codec::PRODUCER_VERSION,
            "bac21cc4027896b8d1836133c347d20f",
            "547c36eb928da163ef61d019bad669860d519f2da0b67737c9a3920e831c9196",
        )
        .unwrap();
        let rendered = build
            .render_observer("/Applications/Libra.app/Contents/MacOS/libra")
            .unwrap();
        let policy = build
            .installation_policy(
                "/Applications/Libra.app/Contents/MacOS/libra",
                rendered.as_bytes(),
            )
            .unwrap();
        let local = LocalBinding {
            installation: Installation {
                version: 1,
                build_identity: policy.build_identity.clone(),
                producer_binary_digest:
                    "547c36eb928da163ef61d019bad669860d519f2da0b67737c9a3920e831c9196".into(),
                issuer_public_key: public.clone(),
                key_generation: 1,
                policy_digest: hex::encode(Sha256::digest(codec::policy_bytes(&policy).unwrap())),
                policy_descriptor: policy,
            },
            producer_version: codec::PRODUCER_VERSION,
            key_dir: "/synthetic/private".into(),
            command_path: "/synthetic/libra".into(),
            observer_executable_sha256: "0".repeat(64),
        };
        let original = json!({"version":1,"challenge":challenge,"issuer":issuer,"publicKey":public,"keyGeneration":1,
            "buildIdentity":local.installation.build_identity,"producerVersion":codec::PRODUCER_VERSION,
            "producerBinaryDigest":local.installation.producer_binary_digest,"policyDigest":local.installation.policy_digest,"signature":""});
        let signed = |mut wire: Value, domain: &[u8]| {
            let mut preimage = domain.to_vec();
            preimage.extend((wire["version"].as_u64().unwrap() as u32).to_be_bytes());
            for name in ["challenge", "issuer", "publicKey"] {
                preimage.extend(hex::decode(wire[name].as_str().unwrap()).unwrap());
            }
            preimage.extend((wire["keyGeneration"].as_u64().unwrap() as u32).to_be_bytes());
            preimage.extend(hex::decode(wire["buildIdentity"].as_str().unwrap()).unwrap());
            let version = wire["producerVersion"].as_str().unwrap().as_bytes();
            preimage.extend((version.len() as u32).to_be_bytes());
            preimage.extend(version);
            for name in ["producerBinaryDigest", "policyDigest"] {
                preimage.extend(hex::decode(wire[name].as_str().unwrap()).unwrap());
            }
            wire["signature"] = json!(STANDARD.encode(key.sign(&preimage).as_ref()));
            let mut bytes = serde_json::to_vec(&wire).unwrap();
            bytes.push(b'\n');
            bytes
        };
        let domain = b"libra-opencode-native-origin-attest-v1\0";
        let valid = signed(original.clone(), domain);
        assert!(verify_challenge(&valid, &challenge, &local).is_ok());
        for (field, value) in [
            ("challenge", json!("2".repeat(64))),
            ("version", json!(2)),
            ("keyGeneration", json!(2)),
            ("issuer", json!("3".repeat(64))),
            ("publicKey", json!("4".repeat(64))),
            ("buildIdentity", json!("5".repeat(32))),
            ("producerVersion", json!("2.0.26")),
            ("producerBinaryDigest", json!("6".repeat(64))),
            ("policyDigest", json!("7".repeat(64))),
        ] {
            let mut changed = original.clone();
            changed[field] = value;
            assert!(
                verify_challenge(&signed(changed, domain), &challenge, &local).is_err(),
                "even an authentic signature cannot change the requested installation binding"
            );
        }
        assert!(
            verify_challenge(
                &signed(
                    original.clone(),
                    b"libra-opencode-native-origin-receipt-v1\0"
                ),
                &challenge,
                &local
            )
            .is_err()
        );
        let mut bad: Value = serde_json::from_slice(&valid).unwrap();
        bad["signature"] = json!(STANDARD.encode([0u8; 64]));
        let mut bytes = serde_json::to_vec(&bad).unwrap();
        bytes.push(b'\n');
        assert!(verify_challenge(&bytes, &challenge, &local).is_err());
        bad["extra"] = json!(1);
        let mut bytes = serde_json::to_vec(&bad).unwrap();
        bytes.push(b'\n');
        assert!(verify_challenge(&bytes, &challenge, &local).is_err());
        let mut trailing = valid.clone();
        trailing.extend(b"{}\n");
        assert!(verify_challenge(&trailing, &challenge, &local).is_err());
        assert!(verify_challenge(&valid, &"8".repeat(64), &local).is_err());

        let build = approved_build::lookup(
            codec::ANTHROPIC_PRODUCER_VERSION,
            "78c1f19e9249da8d0143a079f90aec9a",
            "28174e893a2b63b75f58af4c77212747f6e1eebce5dd0252cfe64847b342615c",
        )
        .unwrap();
        let rendered = build
            .render_observer("/Applications/Libra.app/Contents/MacOS/libra")
            .unwrap();
        let policy = build
            .installation_policy(
                "/Applications/Libra.app/Contents/MacOS/libra",
                rendered.as_bytes(),
            )
            .unwrap();
        let mut anthropic = local;
        anthropic.producer_version = build.identity().producer_version;
        anthropic.installation.build_identity = build.identity().build_identity.into();
        anthropic.installation.producer_binary_digest = build.identity().binary_sha256.into();
        anthropic.installation.policy_digest =
            hex::encode(Sha256::digest(codec::policy_bytes(&policy).unwrap()));
        anthropic.installation.policy_descriptor = policy;
        assert!(verify_challenge(&valid, &challenge, &anthropic).is_err());
        let mut claimed = original;
        claimed["buildIdentity"] = json!(anthropic.installation.build_identity);
        claimed["producerVersion"] = json!(anthropic.producer_version);
        claimed["producerBinaryDigest"] = json!(anthropic.installation.producer_binary_digest);
        claimed["policyDigest"] = json!(anthropic.installation.policy_digest);
        assert!(verify_challenge(&signed(claimed, domain), &challenge, &anthropic).is_ok());
    }
}
