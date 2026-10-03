//! Single owner of repository-private capture integrity keys.
//!
//! Existing ingress/source commitments keep their key path and byte-level
//! domains. Pending-envelope verification never initializes or repairs a key.

#[cfg(unix)]
use std::{
    ffi::{CString, OsStr},
    fs,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt},
        },
    },
    path::PathBuf,
    time::Duration,
};
use std::{
    path::Path,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
#[cfg(unix)]
use ring::{
    hmac,
    rand::{SecureRandom, SystemRandom},
};
use thiserror::Error;

use crate::internal::ai::capture_scope::CaptureScope;

/// Fixed, path-free capability result safe to render to a hook host.
pub(crate) const CAPTURE_UNSUPPORTED_PLATFORM_REMEDY: &str = "Session Capture is unavailable on this platform because secure repository-private key initialization requires Unix descriptor-relative no-replace file APIs; run the hook on a Unix host";

#[derive(Debug, Error)]
#[error("{CAPTURE_UNSUPPORTED_PLATFORM_REMEDY}")]
pub(crate) struct CaptureDedupUnsupportedPlatform;

#[cfg(unix)]
pub(crate) const CAPTURE_DEDUP_SECRET_DIR: &str = "private";
#[cfg(unix)]
pub(crate) const CAPTURE_DEDUP_SECRET_FILE: &str = "agent-capture-dedup-v1.key";
pub(crate) const CAPTURE_DEDUP_SECRET_BYTES: usize = 32;

/// The only non-ingress uses of the repository-private capture key. Keeping
/// the domain choice typed prevents a new caller from accidentally reusing an
/// ingress replay HMAC output as durable source provenance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureSourceCommitmentDomain {
    /// Stable provenance for one redacted snapshot's content. Live and
    /// historical import intentionally share this domain so the same
    /// authorized redacted bytes have the same durable snapshot commitment.
    SnapshotContentV2,
    /// Import ownership is deliberately separate from snapshot content: it
    /// binds the authorized locator/session preimage used by V2 migration and
    /// recovery markers.
    ImportSourceV2,
    SubagentSourceV2,
}

impl CaptureSourceCommitmentDomain {
    fn tag(self) -> &'static [u8] {
        match self {
            Self::SnapshotContentV2 => b"libra-agent-snapshot-content-hmac-v2\0",
            Self::ImportSourceV2 => b"libra-agent-import-source-hmac-v2\0",
            Self::SubagentSourceV2 => b"libra-subagent-source-hmac-v2\0",
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::SnapshotContentV2 | Self::ImportSourceV2 => "source/hmac-v2/",
            Self::SubagentSourceV2 => "source/subagent-hmac-v2/",
        }
    }
}

#[cfg(unix)]
const PENDING_ENVELOPE_MAC_PREFIX: &str = "pending-envelope/hmac-v1/";
#[cfg(unix)]
const PENDING_ENVELOPE_MAC_DOMAIN: &[u8] = b"libra-agent-pending-envelope-hmac-v1\0";
const PENDING_ENVELOPE_MAX_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
enum PrivateArtifactMacDomain {
    Envelope,
    SessionAlias,
}

impl PrivateArtifactMacDomain {
    fn byte_cap(self) -> usize {
        match self {
            Self::Envelope => PENDING_ENVELOPE_MAX_BYTES,
            Self::SessionAlias => 8 * 1024,
        }
    }

    #[cfg(unix)]
    fn prefix(self) -> &'static str {
        match self {
            Self::Envelope => PENDING_ENVELOPE_MAC_PREFIX,
            Self::SessionAlias => "pending-alias/hmac-v1/",
        }
    }

    #[cfg(unix)]
    fn tag(self) -> &'static [u8] {
        match self {
            Self::Envelope => PENDING_ENVELOPE_MAC_DOMAIN,
            Self::SessionAlias => b"libra-agent-pending-alias-hmac-v1\0",
        }
    }
}

/// Association authentication is separate from source and envelope authority.
/// As with replay, this operation only reads an already initialized key.
pub(crate) async fn authenticate_pending_alias_in_scope_until<C: sea_orm::ConnectionTrait>(
    conn: &C,
    scope: &CaptureScope,
    storage_path: &Path,
    authorized_root: &Path,
    body: &[u8],
    expected_mac: Option<&str>,
    deadline: Instant,
) -> Result<String> {
    let storage_path = scope
        .assert_capture_key_storage_binding_until(conn, storage_path, authorized_root, deadline)
        .await?;
    authenticate_private_artifact_existing_key(
        &storage_path,
        body,
        expected_mac,
        deadline,
        PrivateArtifactMacDomain::SessionAlias,
    )
}

/// Authenticate the canonical envelope without granting source authority.
/// The unkeyed content preimage remains transient; only the domain-separated
/// MAC may enter the private artifact codec. Verification uses ring's
/// constant-time comparison, never equality on attacker-controlled strings.
pub(crate) async fn authenticate_pending_envelope_in_scope_until<C: sea_orm::ConnectionTrait>(
    conn: &C,
    scope: &CaptureScope,
    storage_path: &Path,
    authorized_root: &Path,
    envelope: &[u8],
    expected_mac: Option<&str>,
    deadline: Instant,
) -> Result<String> {
    let storage_path = scope
        .assert_capture_key_storage_binding_until(conn, storage_path, authorized_root, deadline)
        .await?;
    authenticate_pending_envelope_existing_key(&storage_path, envelope, expected_mac, deadline)
}

fn authenticate_pending_envelope_existing_key(
    storage_path: &Path,
    envelope: &[u8],
    expected_mac: Option<&str>,
    deadline: Instant,
) -> Result<String> {
    authenticate_private_artifact_existing_key(
        storage_path,
        envelope,
        expected_mac,
        deadline,
        PrivateArtifactMacDomain::Envelope,
    )
}

fn authenticate_private_artifact_existing_key(
    storage_path: &Path,
    envelope: &[u8],
    expected_mac: Option<&str>,
    deadline: Instant,
    domain: PrivateArtifactMacDomain,
) -> Result<String> {
    ensure_capture_source_commitment_deadline(deadline)?;
    if envelope.len() > domain.byte_cap() {
        bail!(
            "pending capture artifact exceeds its safe authentication limit; run libra agent doctor"
        );
    }
    #[cfg(unix)]
    {
        // This read neither creates directories/files nor tightens modes,
        // reclaims staging entries, or waits for an initializer. A lost key
        // must remain visible damage, not silently rotate capture identity.
        let private = CaptureDedupPrivateDir::open_path_read_only(
            &storage_path.join(CAPTURE_DEDUP_SECRET_DIR),
            "key directory",
        )?;
        let secret = private.read_secret()?.context(
            "repository-private capture key is missing; run libra agent doctor before retrying recovery",
        )?;
        ensure_capture_source_commitment_deadline(deadline)?;
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        for chunk in envelope.chunks(512 * 1024) {
            ensure_capture_source_commitment_deadline(deadline)?;
            digest.update(chunk);
        }
        let digest = digest.finish();
        let mut message = Vec::with_capacity(domain.tag().len() + digest.as_ref().len());
        message.extend_from_slice(domain.tag());
        message.extend_from_slice(digest.as_ref());
        let key = hmac::Key::new(hmac::HMAC_SHA256, &secret);
        if let Some(expected) = expected_mac {
            let value = expected
                .strip_prefix(domain.prefix())
                .filter(|value| {
                    value.len() == 64
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                })
                .context("pending capture artifact has an invalid authentication tag")?;
            let mut tag = [0u8; 32];
            hex::decode_to_slice(value, &mut tag)
                .context("pending capture artifact has an invalid authentication tag")?;
            hmac::verify(&key, &message, &tag).map_err(|_| {
                anyhow!("pending capture artifact authentication failed; run libra agent doctor")
            })?;
        }
        ensure_capture_source_commitment_deadline(deadline)?;
        Ok(format!(
            "{}{}",
            domain.prefix(),
            hex::encode(hmac::sign(&key, &message).as_ref())
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = (storage_path, envelope, expected_mac);
        Err(CaptureDedupUnsupportedPlatform.into())
    }
}
#[cfg(unix)]
/// Staging is deliberately a dedicated child of `private`, rather than the
/// generic `.tmp*` namespace. A killed helper can therefore leave only an
/// attributable entry behind; cleanup never needs to guess whether a sibling
/// belongs to another local tool.
const CAPTURE_DEDUP_TEMP_DIR: &str = "agent-capture-dedup-v1.tmp";
#[cfg(unix)]
const CAPTURE_DEDUP_TEMP_PREFIX: &str = "agent-capture-dedup-v1-";
#[cfg(unix)]
const CAPTURE_DEDUP_TEMP_LOCK_FILE: &str = ".agent-capture-dedup-v1.lock";
#[cfg(unix)]
const CAPTURE_DEDUP_TEMP_RANDOM_HEX_BYTES: usize = 16;
#[cfg(unix)]
const CAPTURE_DEDUP_TEMP_CREATE_ATTEMPTS: usize = 16;
#[cfg(unix)]
const CAPTURE_DEDUP_TEMP_CLEANUP_ENTRY_CAP: usize = 256;
#[cfg(unix)]
const CAPTURE_DEDUP_TEMP_LOCK_WAIT_SLICE: Duration = Duration::from_millis(10);
#[cfg(unix)]
const CAPTURE_DEDUP_TEMP_LOCK_MAX_WAIT_WITHOUT_DEADLINE: Duration = Duration::from_secs(30);

/// Load the repository-private HMAC key used to make ingress replay IDs.
///
/// The key is not a capture artifact: it is never added to `SessionState`, a
/// checkpoint, history, tracing, export, or the database. We create it only
/// after the reported cwd has proven it belongs to the active worktree, and
/// use a pinned, owner-only directory plus `openat`/`O_NOFOLLOW` on Unix so
/// another local path cannot replace it with an attacker-controlled oracle
/// key. The scoped helper may be SIGKILLed while writing its first key: Unix
/// staging entries hold an advisory lock while live, so the next helper can
/// reclaim only released, attributable entries without racing an active one.
/// Non-Unix targets fail closed until they have an equivalent descriptor-relative
/// no-replace publication primitive; they never fall back to path-based key I/O.
#[cfg(test)]
pub(crate) fn load_capture_dedup_secret(
    storage_path: &Path,
) -> Result<[u8; CAPTURE_DEDUP_SECRET_BYTES]> {
    load_capture_dedup_secret_with_mutation_deadline(storage_path, None)
}

/// The helper's wall-clock mutation deadline, when present, is host-derived
/// rather than provider-derived. Thread it into the key initializer so a
/// contended staging lock cannot turn an expired helper into a late writer.
pub(crate) fn load_capture_dedup_secret_with_mutation_deadline(
    storage_path: &Path,
    mutation_deadline_millis: Option<i64>,
) -> Result<[u8; CAPTURE_DEDUP_SECRET_BYTES]> {
    #[cfg(unix)]
    {
        load_capture_dedup_secret_unix(storage_path, mutation_deadline_millis)
    }
    #[cfg(not(unix))]
    {
        load_capture_dedup_secret_non_unix_unsupported(storage_path, mutation_deadline_millis)
    }
}

/// Derive a durable, non-enumerable source commitment after the caller has
/// revalidated its repository scope and authorized root. The function repeats
/// the root-to-storage binding internally, so a caller cannot accidentally
/// derive against a sibling repository's secret after only checking a live
/// workspace fence. The input is already a fixed-size, transient SHA-256
/// preimage; raw locators, provider IDs, and the key itself never enter this
/// API's return value or a helper wire.
pub(crate) async fn derive_capture_source_commitment_in_scope_until<C: sea_orm::ConnectionTrait>(
    conn: &C,
    scope: &CaptureScope,
    storage_path: &Path,
    authorized_root: &Path,
    domain: CaptureSourceCommitmentDomain,
    preimage: &[u8; 32],
    deadline: Instant,
) -> Result<String> {
    // Scope resolution and lease validation both query SQLite.  The caller's
    // deadline is absolute, so wrap the entire capability acquisition rather
    // than merely checking before and after each await; a contended database
    // must not let an import/subagent writer mint a late commitment.
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
        ensure_capture_source_commitment_deadline(deadline)?;
        let storage_path = scope
            .assert_capture_key_storage_binding_until(conn, storage_path, authorized_root, deadline)
            .await?;
        let mutation_deadline_millis = capture_source_commitment_deadline_millis(deadline)?;
        ensure_capture_dedup_mutation_deadline(Some(mutation_deadline_millis))?;
        let secret = load_capture_dedup_secret_with_mutation_deadline(
            &storage_path,
            Some(mutation_deadline_millis),
        )?;
        ensure_capture_dedup_mutation_deadline(Some(mutation_deadline_millis))?;
        #[cfg(unix)]
        {
            let key = hmac::Key::new(hmac::HMAC_SHA256, &secret);
            let mut context = hmac::Context::with_key(&key);
            context.update(domain.tag());
            context.update(preimage);
            Ok(format!(
                "{}{}",
                domain.prefix(),
                hex::encode(context.sign().as_ref())
            ))
        }
        #[cfg(not(unix))]
        {
            let _ = (secret, domain, preimage);
            Err(CaptureDedupUnsupportedPlatform.into())
        }
    })
    .await
    .map_err(|_| {
        anyhow!(
            crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError::DeadlineElapsed
        )
    })?
}

/// Derive the repository-scoped commitment for a redacted snapshot digest.
/// Keeping the snapshot-content domain here prevents live capture, export,
/// import, and pending writers from accidentally selecting different domains.
pub(crate) async fn derive_snapshot_content_commitment_in_scope_until<
    C: sea_orm::ConnectionTrait,
>(
    conn: &C,
    scope: &CaptureScope,
    storage_path: &Path,
    authorized_root: &Path,
    preimage: &[u8; 32],
    deadline: Instant,
) -> Result<String> {
    derive_capture_source_commitment_in_scope_until(
        conn,
        scope,
        storage_path,
        authorized_root,
        CaptureSourceCommitmentDomain::SnapshotContentV2,
        preimage,
        deadline,
    )
    .await
}

fn ensure_capture_source_commitment_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(
            crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError::DeadlineElapsed
                .into(),
        );
    }
    Ok(())
}

/// Convert the parent's monotonic import deadline into the key lifecycle's
/// host-owned wall-clock mutation gate.  The loader checks this value before
/// every lock wait and namespace mutation; the monotonic check before and
/// after conversion prevents a delayed system clock from extending an import.
fn capture_source_commitment_deadline_millis(deadline: Instant) -> Result<i64> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| {
            crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError::DeadlineElapsed
        })?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock precedes the Unix epoch while deriving capture source commitment")?
        .as_millis();
    let deadline_millis = now
        .checked_add(remaining.as_millis())
        .context("capture source commitment deadline exceeds wall-clock range")?;
    let deadline_millis = i64::try_from(deadline_millis)
        .context("capture source commitment deadline exceeds persistent range")?;
    if Instant::now() >= deadline {
        return Err(
            crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError::DeadlineElapsed
                .into(),
        );
    }
    Ok(deadline_millis)
}

/// A path-only implementation cannot establish that a hostile local process
/// did not replace either endpoint between validation and no-replace publish.
/// Do not create a key, staging directory, or temp file on targets where the
/// runtime lacks the Unix descriptor-relative primitives used above.
#[cfg(not(unix))]
fn load_capture_dedup_secret_non_unix_unsupported(
    _storage_path: &Path,
    _mutation_deadline_millis: Option<i64>,
) -> Result<[u8; CAPTURE_DEDUP_SECRET_BYTES]> {
    // This branch cannot mutate a namespace, so the fixed capability result
    // must take precedence over a deadline check. Otherwise an expired host
    // budget would hide the only safe external diagnostic for this platform.
    Err(CaptureDedupUnsupportedPlatform.into())
}

#[cfg(unix)]
fn load_capture_dedup_secret_unix(
    storage_path: &Path,
    mutation_deadline_millis: Option<i64>,
) -> Result<[u8; CAPTURE_DEDUP_SECRET_BYTES]> {
    let private_dir = storage_path.join(CAPTURE_DEDUP_SECRET_DIR);
    ensure_capture_dedup_mutation_deadline(mutation_deadline_millis)?;
    let private_dir = CaptureDedupPrivateDir::open_or_create(
        &private_dir,
        "key directory",
        mutation_deadline_millis,
    )?;
    ensure_capture_dedup_mutation_deadline(mutation_deadline_millis)?;
    let staging_dir = private_dir.open_or_create_child(
        CAPTURE_DEDUP_TEMP_DIR,
        "staging directory",
        mutation_deadline_millis,
    )?;
    let wait_without_deadline = Instant::now() + CAPTURE_DEDUP_TEMP_LOCK_MAX_WAIT_WITHOUT_DEADLINE;
    loop {
        ensure_capture_dedup_mutation_deadline(mutation_deadline_millis)?;
        if let Some(staging_lock) =
            staging_dir.try_acquire_staging_lock(mutation_deadline_millis)?
        {
            return load_capture_dedup_secret_unix_locked(
                &private_dir,
                &staging_dir,
                &staging_lock,
                mutation_deadline_millis,
            );
        }
        // A concurrent helper owns the staging lock. It may already have
        // atomically linked a complete final key, so read only that pinned
        // final name while waiting; never scan or create a matching temp
        // entry without the lock.
        if let Some(secret) = read_capture_dedup_secret_unix(&private_dir)? {
            return Ok(secret);
        }
        if mutation_deadline_millis.is_none() && Instant::now() >= wait_without_deadline {
            bail!(
                "repository-private agent capture key initialization remained busy for 30 seconds"
            );
        }
        std::thread::sleep(CAPTURE_DEDUP_TEMP_LOCK_WAIT_SLICE);
    }
}

#[cfg(unix)]
fn load_capture_dedup_secret_unix_locked(
    private_dir: &CaptureDedupPrivateDir,
    staging_dir: &CaptureDedupPrivateDir,
    staging_lock: &CaptureDedupStagingLock,
    mutation_deadline_millis: Option<i64>,
) -> Result<[u8; CAPTURE_DEDUP_SECRET_BYTES]> {
    // A successful publisher can be killed after `linkat` and before it
    // unlinks staging. Reclaim that residue only while the child is still
    // allowed to mutate local state. This must run before reading the final
    // key so an already-published key also drains its one remaining staging
    // hard link on the normal recovery path.
    ensure_capture_dedup_mutation_deadline(mutation_deadline_millis)?;
    // Cleanup accepts only our exact random-name grammar, a regular 0600 file
    // owned by this euid, and a released lock; unknown entries are untouched.
    staging_dir.remove_released_temp_files(private_dir, staging_lock, mutation_deadline_millis)?;
    match read_capture_dedup_secret_unix(private_dir) {
        Ok(Some(secret)) => return Ok(secret),
        Ok(None) => {}
        // This key format has not shipped yet. A malformed final file is
        // therefore tampering or local damage, not a recoverable legacy
        // record: fail closed rather than silently rotate an identity key and
        // make prior dedup receipts unverifiable.
        Err(error) => return Err(error),
    }
    // The wait/cleanup path above may consume most of the managed helper
    // budget. Recheck immediately before the first newly-created key entry.
    ensure_capture_dedup_mutation_deadline(mutation_deadline_millis)?;
    let mut generated = [0u8; CAPTURE_DEDUP_SECRET_BYTES];
    SystemRandom::new()
        .fill(&mut generated)
        .map_err(|_| anyhow!("generate repository-private agent capture key"))?;

    // `linkat` is a no-replace publication into the already-pinned private
    // directory. Its source stays in the staging directory until the final
    // name has been synced, which preserves the old failure semantics while
    // making any SIGKILL residue safe for a later helper to reclaim.
    let mut temporary = staging_dir
        .create_temp_file(staging_lock, mutation_deadline_millis)
        .context("create temporary repository-private agent capture key")?;
    ensure_capture_dedup_mutation_deadline(mutation_deadline_millis)?;
    temporary
        .file
        .write_all(&generated)
        .context("write temporary repository-private agent capture key")?;
    temporary
        .file
        .sync_all()
        .context("flush temporary repository-private agent capture key")?;
    ensure_capture_dedup_mutation_deadline(mutation_deadline_millis)?;
    match temporary.publish_noclobber(private_dir) {
        Ok(()) => Ok(generated),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            wait_for_capture_dedup_secret_unix(private_dir, mutation_deadline_millis)
        }
        Err(error) => Err(error).context("publish repository-private agent capture key"),
    }
}

#[cfg(unix)]
fn wait_for_capture_dedup_secret_unix(
    private_dir: &CaptureDedupPrivateDir,
    mutation_deadline_millis: Option<i64>,
) -> Result<[u8; CAPTURE_DEDUP_SECRET_BYTES]> {
    let wait_without_deadline = Instant::now() + CAPTURE_DEDUP_TEMP_LOCK_MAX_WAIT_WITHOUT_DEADLINE;
    loop {
        ensure_capture_dedup_mutation_deadline(mutation_deadline_millis)?;
        if let Some(secret) = read_capture_dedup_secret_unix(private_dir)? {
            return Ok(secret);
        }
        if mutation_deadline_millis.is_none() && Instant::now() >= wait_without_deadline {
            bail!(
                "repository-private agent capture key was not initialized after waiting 30 seconds for a concurrent publisher"
            );
        }
        std::thread::sleep(CAPTURE_DEDUP_TEMP_LOCK_WAIT_SLICE);
    }
}

#[cfg(unix)]
struct CaptureDedupPrivateDir {
    file: fs::File,
    /// Used only to enumerate candidate names. Each subsequent open/unlink is
    /// relative to `file`, so a path swap can at worst supply ignored names.
    path: PathBuf,
}

#[cfg(unix)]
impl CaptureDedupPrivateDir {
    fn open_or_create(
        path: &Path,
        label: &str,
        mutation_deadline_millis: Option<i64>,
    ) -> Result<Self> {
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        // The caller may be resumed after its last outer deadline check.
        // Keep this check adjacent to mkdir so an expired helper cannot
        // create the private namespace after its host callback returned.
        ensure_capture_dedup_mutation_deadline(mutation_deadline_millis)?;
        match builder.create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("create repository-private agent capture {label}"));
            }
        }
        Self::open_path(path, label, mutation_deadline_millis)
    }

    fn open_or_create_child(
        &self,
        name: &str,
        label: &str,
        mutation_deadline_millis: Option<i64>,
    ) -> Result<Self> {
        let name = capture_dedup_c_string(name.as_bytes())?;
        // See `open_or_create`: a deadline check immediately before mkdirat
        // is required even though the caller checked before entering here.
        ensure_capture_dedup_mutation_deadline(mutation_deadline_millis)?;
        // SAFETY: `self.file` is a live directory descriptor and `name` is a
        // NUL-terminated constant protocol component without a slash.
        let result = unsafe { libc::mkdirat(self.file.as_raw_fd(), name.as_ptr(), 0o700) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error)
                    .with_context(|| format!("create repository-private agent capture {label}"));
            }
        }
        Self::open_at(
            &self.file,
            self.path.join(name.to_string_lossy().as_ref()),
            &name,
            label,
            mutation_deadline_millis,
        )
    }

    fn open_path(path: &Path, label: &str, mutation_deadline_millis: Option<i64>) -> Result<Self> {
        Self::open_path_with_access(path, label, mutation_deadline_millis, false)
    }

    fn open_path_read_only(path: &Path, label: &str) -> Result<Self> {
        Self::open_path_with_access(path, label, None, true)
    }

    fn open_path_with_access(
        path: &Path,
        label: &str,
        mutation_deadline_millis: Option<i64>,
        read_only: bool,
    ) -> Result<Self> {
        let path_c = capture_dedup_c_string(path.as_os_str().as_bytes())?;
        // SAFETY: `path_c` is NUL-terminated and remains live for the call.
        let raw = unsafe {
            libc::open(
                path_c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("open repository-private agent capture {label}"));
        }
        // SAFETY: `open` returned a fresh owned descriptor above.
        let file = unsafe { fs::File::from_raw_fd(raw) };
        if read_only {
            Self::validate_read_only(&file, label)?;
        } else {
            Self::validate(&file, label, mutation_deadline_millis)?;
        }
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    fn open_at(
        parent: &fs::File,
        path: PathBuf,
        name: &CString,
        label: &str,
        mutation_deadline_millis: Option<i64>,
    ) -> Result<Self> {
        // SAFETY: `parent` is a live directory descriptor and `name` is a
        // NUL-terminated child component created by this module.
        let raw = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("open repository-private agent capture {label}"));
        }
        // SAFETY: `openat` returned a fresh owned descriptor above.
        let file = unsafe { fs::File::from_raw_fd(raw) };
        Self::validate(&file, label, mutation_deadline_millis)?;
        Ok(Self { file, path })
    }

    fn validate(file: &fs::File, label: &str, mutation_deadline_millis: Option<i64>) -> Result<()> {
        let metadata = file
            .metadata()
            .with_context(|| format!("inspect repository-private agent capture {label}"))?;
        if !metadata.file_type().is_dir() {
            bail!("repository-private agent capture {label} is not a real directory");
        }
        // SAFETY: `geteuid` has no preconditions and is process-local.
        let euid = unsafe { libc::geteuid() };
        if metadata.uid() != euid {
            bail!("repository-private agent capture {label} is not owned by this user");
        }
        let mode = metadata.mode() & 0o777;
        // Do not 'fix' a directory another local user may have populated:
        // they could already have planted a final key or staging candidate.
        if mode & 0o022 != 0 {
            bail!("repository-private agent capture {label} is writable by group or other users");
        }
        // The fchmod below is a real namespace mutation, so a delayed helper
        // must prove its host-issued wall-clock deadline again at this exact
        // edge rather than relying on an earlier outer check.
        ensure_capture_dedup_mutation_deadline(mutation_deadline_millis)?;
        // SAFETY: `file` is the pinned directory descriptor we just checked.
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o700) } != 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!("restrict repository-private agent capture {label} to 0700")
            });
        }
        let metadata = file
            .metadata()
            .with_context(|| format!("reinspect repository-private agent capture {label}"))?;
        if metadata.uid() != euid || metadata.mode() & 0o777 != 0o700 {
            bail!("repository-private agent capture {label} could not be secured to 0700");
        }
        Ok(())
    }

    fn validate_read_only(file: &fs::File, label: &str) -> Result<()> {
        let metadata = file
            .metadata()
            .with_context(|| format!("inspect repository-private agent capture {label}"))?;
        // SAFETY: geteuid has no preconditions and is process-local.
        let euid = unsafe { libc::geteuid() };
        if !metadata.file_type().is_dir()
            || metadata.uid() != euid
            || metadata.mode() & 0o777 != 0o700
        {
            bail!(
                "repository-private agent capture {label} must be an owner-only 0700 directory; run libra agent doctor before recovery"
            );
        }
        Ok(())
    }

    fn read_secret(&self) -> Result<Option<[u8; CAPTURE_DEDUP_SECRET_BYTES]>> {
        let name = capture_dedup_c_string(CAPTURE_DEDUP_SECRET_FILE.as_bytes())?;
        let file = match self.open_no_follow(&name, false) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("open repository-private agent capture key"),
        };
        let metadata = file
            .metadata()
            .context("inspect repository-private agent capture key")?;
        if !metadata.file_type().is_file() {
            bail!("repository-private agent capture key must be a regular file");
        }
        // SAFETY: `geteuid` has no preconditions and is process-local.
        let euid = unsafe { libc::geteuid() };
        if metadata.uid() != euid {
            bail!("repository-private agent capture key is not owned by this user");
        }
        if metadata.mode() & 0o777 != 0o600 {
            bail!("repository-private agent capture key must have permissions 0600");
        }
        let mut bytes = Vec::with_capacity(CAPTURE_DEDUP_SECRET_BYTES + 1);
        file.take((CAPTURE_DEDUP_SECRET_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .context("read repository-private agent capture key")?;
        if bytes.len() != CAPTURE_DEDUP_SECRET_BYTES {
            bail!("repository-private agent capture key has an invalid length");
        }
        let mut secret = [0u8; CAPTURE_DEDUP_SECRET_BYTES];
        secret.copy_from_slice(&bytes);
        Ok(Some(secret))
    }

    /// Take the one staging lock before any matching temporary entry is
    /// enumerated or created. The lock file itself is pinned/no-follow and
    /// validated like key material; a pre-planted abnormal entry is refused
    /// rather than silently replaced.
    fn try_acquire_staging_lock(
        &self,
        mutation_deadline_millis: Option<i64>,
    ) -> Result<Option<CaptureDedupStagingLock>> {
        let name = capture_dedup_c_string(CAPTURE_DEDUP_TEMP_LOCK_FILE.as_bytes())?;
        // O_CREAT is a mutation even when the usual path finds an existing
        // lock. Check at the syscall edge so a delayed helper cannot create
        // the lock namespace after the host deadline.
        ensure_capture_dedup_mutation_deadline(mutation_deadline_millis)?;
        // SAFETY: `self.file` is a pinned directory descriptor and `name` is
        // a NUL-terminated fixed protocol component.
        let raw = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if raw < 0 {
            let error = std::io::Error::last_os_error();
            // During first-use bootstrap, Darwin/APFS can transiently report
            // ENOENT when two helpers concurrently use O_CREAT|O_NOFOLLOW
            // for this exact lock entry. Treat only that raw errno as
            // contention: the outer loop makes a pinned final-key read,
            // reapplies its deadline gate, and retries before any write.
            // Other open errors and every later metadata validation failure
            // remain fail-closed.
            if capture_dedup_staging_lock_open_is_bootstrap_transient(&error) {
                return Ok(None);
            }
            return Err(error).context("open repository-private agent capture staging lock");
        }
        // SAFETY: `openat` returned a fresh owned descriptor above.
        let file = unsafe { fs::File::from_raw_fd(raw) };
        let metadata = file
            .metadata()
            .context("inspect repository-private agent capture staging lock")?;
        // SAFETY: `geteuid` has no preconditions and is process-local.
        let euid = unsafe { libc::geteuid() };
        if !metadata.file_type().is_file()
            || metadata.uid() != euid
            || metadata.mode() & 0o777 != 0o600
            || metadata.nlink() != 1
        {
            bail!(
                "repository-private agent capture staging lock is not a secure regular 0600 file"
            );
        }
        if capture_dedup_try_lock(&file)? {
            Ok(Some(CaptureDedupStagingLock { _file: file }))
        } else {
            Ok(None)
        }
    }

    fn create_temp_file(
        &self,
        _staging_lock: &CaptureDedupStagingLock,
        mutation_deadline_millis: Option<i64>,
    ) -> Result<CaptureDedupStagingFile> {
        for _ in 0..CAPTURE_DEDUP_TEMP_CREATE_ATTEMPTS {
            let mut random = [0u8; CAPTURE_DEDUP_TEMP_RANDOM_HEX_BYTES];
            SystemRandom::new().fill(&mut random).map_err(|_| {
                anyhow!("generate temporary repository-private agent capture key name")
            })?;
            let name = capture_dedup_c_string(
                format!("{CAPTURE_DEDUP_TEMP_PREFIX}{}", hex::encode(random)).as_bytes(),
            )?;
            // This is the exact mutation edge: a scoped helper that spent its
            // budget waiting for another writer must not create a new entry.
            ensure_capture_dedup_mutation_deadline(mutation_deadline_millis)?;
            // SAFETY: `self.file` is pinned, `name` is a NUL-terminated
            // protocol component, and O_EXCL prevents replacement of an
            // existing entry.
            let raw = unsafe {
                libc::openat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if raw < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(error).context("create temporary repository-private agent capture key");
            }
            // SAFETY: `openat` returned a fresh owned descriptor above.
            let file = unsafe { fs::File::from_raw_fd(raw) };
            let metadata = file
                .metadata()
                .context("inspect temporary repository-private agent capture key")?;
            // SAFETY: `geteuid` has no preconditions and is process-local.
            let euid = unsafe { libc::geteuid() };
            if !metadata.file_type().is_file()
                || metadata.uid() != euid
                || metadata.mode() & 0o777 != 0o600
                || metadata.nlink() != 1
            {
                if ensure_capture_dedup_mutation_deadline(mutation_deadline_millis).is_ok() {
                    let _ = unlink_capture_dedup_name(&self.file, &name);
                }
                bail!("temporary repository-private agent capture key could not be secured");
            }
            if !capture_dedup_try_lock(&file)? {
                if ensure_capture_dedup_mutation_deadline(mutation_deadline_millis).is_ok() {
                    let _ = unlink_capture_dedup_name(&self.file, &name);
                }
                bail!("temporary repository-private agent capture key lock is unavailable");
            }
            return Ok(CaptureDedupStagingFile {
                directory: self
                    .file
                    .try_clone()
                    .context("duplicate temporary repository-private agent capture directory")?,
                name,
                file,
                remove_on_drop: true,
                mutation_deadline_millis,
            });
        }
        bail!("could not allocate a unique temporary repository-private agent capture key")
    }

    fn remove_released_temp_files(
        &self,
        private_dir: &CaptureDedupPrivateDir,
        _staging_lock: &CaptureDedupStagingLock,
        mutation_deadline_millis: Option<i64>,
    ) -> Result<()> {
        // Enumeration is best-effort maintenance, never a capture authority.
        // `path` could have been renamed after we pinned `file`; only names
        // from it are consumed, and every dangerous operation stays relative
        // to the pinned descriptor below. A cap also prevents such a swapped
        // path from turning one hook callback into an unbounded directory walk.
        let Ok(entries) = fs::read_dir(&self.path) else {
            return Ok(());
        };
        for entry in entries.take(CAPTURE_DEDUP_TEMP_CLEANUP_ENTRY_CAP) {
            let Ok(entry) = entry else {
                continue;
            };
            let name = entry.file_name();
            if !capture_dedup_temp_name_is_valid(&name) {
                continue;
            }
            // All operations after this point are relative to the pinned
            // descriptor. The readdir path is only an untrusted source of a
            // candidate name, never an authority for an unlink target.
            let Ok(name_c) = capture_dedup_c_string(name.as_bytes()) else {
                continue;
            };
            let Ok(file) = self.open_no_follow(&name_c, true) else {
                // A symlink, FIFO, race, or unreadable user entry is not an
                // attributable stale key. Preserve it rather than guessing.
                continue;
            };
            let Ok(metadata) = file.metadata() else {
                continue;
            };
            // SAFETY: `geteuid` has no preconditions and is process-local.
            let euid = unsafe { libc::geteuid() };
            if !metadata.file_type().is_file()
                || metadata.uid() != euid
                || metadata.mode() & 0o777 != 0o600
                || metadata.len() > CAPTURE_DEDUP_SECRET_BYTES as u64
            {
                continue;
            }
            // A helper can be killed after linkat publishes the final key but
            // before it unlinks staging. That leaves exactly two links to the
            // same inode. Reclaim this only when the second link is the
            // pinned final key; every other hard-linked candidate is left
            // alone rather than risking a user-created file.
            if metadata.nlink() != 1
                && (metadata.nlink() != 2 || !private_dir.final_key_matches(&metadata))
            {
                continue;
            }
            let Ok(released) = capture_dedup_try_lock(&file) else {
                // Locking unavailable is not a license to delete: retain the
                // candidate and let a later helper retry conservatively.
                continue;
            };
            if released {
                ensure_capture_dedup_mutation_deadline(mutation_deadline_millis)?;
                let _ = unlink_capture_dedup_name(&self.file, &name_c);
            }
        }
        Ok(())
    }

    fn open_no_follow(&self, name: &CString, writable: bool) -> std::io::Result<fs::File> {
        // O_NONBLOCK makes an injected FIFO safe to classify after open; it
        // can never park capture before this module rejects it as non-regular.
        // SAFETY: `self.file` is a live directory descriptor and `name` is a
        // NUL-terminated one-component entry name.
        let raw = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                if writable {
                    libc::O_RDWR
                } else {
                    libc::O_RDONLY
                } | libc::O_NONBLOCK
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `openat` returned a fresh owned descriptor above.
        Ok(unsafe { fs::File::from_raw_fd(raw) })
    }

    fn sync(&self) -> std::io::Result<()> {
        self.file.sync_all()
    }

    fn final_key_matches(&self, expected: &std::fs::Metadata) -> bool {
        let Ok(name) = capture_dedup_c_string(CAPTURE_DEDUP_SECRET_FILE.as_bytes()) else {
            return false;
        };
        let Ok(file) = self.open_no_follow(&name, false) else {
            return false;
        };
        let Ok(actual) = file.metadata() else {
            return false;
        };
        actual.file_type().is_file()
            && actual.dev() == expected.dev()
            && actual.ino() == expected.ino()
    }
}

#[cfg(unix)]
struct CaptureDedupStagingLock {
    // The descriptor owns the `flock` for the entire interval in which a
    // matching protocol staging name can be created or reclaimed.
    _file: fs::File,
}

#[cfg(unix)]
struct CaptureDedupStagingFile {
    directory: fs::File,
    name: CString,
    file: fs::File,
    remove_on_drop: bool,
    mutation_deadline_millis: Option<i64>,
}

#[cfg(unix)]
impl CaptureDedupStagingFile {
    fn publish_noclobber(&mut self, private_dir: &CaptureDedupPrivateDir) -> std::io::Result<()> {
        let final_name =
            capture_dedup_c_string(CAPTURE_DEDUP_SECRET_FILE.as_bytes()).map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string())
            })?;
        // A sync/write above can return after the host deadline. Keep the
        // no-replace link itself behind the same child-side mutation gate.
        ensure_capture_dedup_mutation_deadline_io(self.mutation_deadline_millis)?;
        // SAFETY: both descriptors are pinned directories, `name` and
        // `final_name` are NUL-terminated single components, and linkat never
        // replaces an existing destination entry.
        let linked = unsafe {
            libc::linkat(
                self.directory.as_raw_fd(),
                self.name.as_ptr(),
                private_dir.file.as_raw_fd(),
                final_name.as_ptr(),
                0,
            )
        };
        if linked != 0 {
            return Err(std::io::Error::last_os_error());
        }
        ensure_capture_dedup_mutation_deadline_io(self.mutation_deadline_millis)?;
        if let Err(error) = private_dir.sync() {
            // The final name may already be durable despite the reported
            // error. Keep the staging bytes for a later conservative cleanup
            // rather than letting Drop erase the only confirmed copy.
            self.remove_on_drop = false;
            return Err(error);
        }
        // Do not report a completed publication while the final key still has
        // the staging hard link. A concurrent reader can safely observe the
        // brief two-link state, but a normal successful helper must either
        // finish its cleanup durably or report the cleanup failure.
        self.unlink()?;
        ensure_capture_dedup_mutation_deadline_io(self.mutation_deadline_millis)?;
        self.directory.sync_all()?;
        Ok(())
    }

    fn unlink(&mut self) -> std::io::Result<()> {
        ensure_capture_dedup_mutation_deadline_io(self.mutation_deadline_millis)?;
        match unlink_capture_dedup_name(&self.directory, &self.name) {
            Ok(()) => {
                self.remove_on_drop = false;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.remove_on_drop = false;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(unix)]
impl Drop for CaptureDedupStagingFile {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = self.unlink();
        }
    }
}

#[cfg(unix)]
fn read_capture_dedup_secret_unix(
    private_dir: &CaptureDedupPrivateDir,
) -> Result<Option<[u8; CAPTURE_DEDUP_SECRET_BYTES]>> {
    private_dir.read_secret()
}

#[cfg(unix)]
fn capture_dedup_temp_name_is_valid(name: &OsStr) -> bool {
    let bytes = name.as_bytes();
    let prefix = CAPTURE_DEDUP_TEMP_PREFIX.as_bytes();
    let Some(suffix) = bytes.strip_prefix(prefix) else {
        return false;
    };
    suffix.len() == CAPTURE_DEDUP_TEMP_RANDOM_HEX_BYTES * 2
        && suffix
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

#[cfg(unix)]
fn capture_dedup_c_string(bytes: &[u8]) -> Result<CString> {
    CString::new(bytes).context("repository-private agent capture path contains NUL")
}

#[cfg(unix)]
fn capture_dedup_try_lock(file: &fs::File) -> std::io::Result<bool> {
    // SAFETY: flock operates on the owned descriptor and does not outlive it.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(false),
        _ => Err(error),
    }
}

#[cfg(unix)]
fn capture_dedup_staging_lock_open_is_bootstrap_transient(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ENOENT)
}

#[cfg(unix)]
fn unlink_capture_dedup_name(directory: &fs::File, name: &CString) -> std::io::Result<()> {
    // SAFETY: `directory` is a live pinned directory descriptor and `name` is
    // a NUL-terminated one-component entry name.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub(crate) fn ensure_scope_binding_mutation_deadline(deadline_millis: i64) -> Result<()> {
    let now_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock precedes the Unix epoch while binding capture scope")?
        .as_millis();
    let now_millis = i64::try_from(now_millis)
        .context("system clock exceeds the capture scope-binding deadline range")?;
    if now_millis >= deadline_millis {
        bail!("capture scope binding deadline elapsed before replay-key mutation");
    }
    Ok(())
}

/// Keep the optional host-issued deadline at the key I/O boundary. This
/// helper deliberately accepts no provider data, so a hook payload cannot
/// shorten or extend the mutation window.
fn ensure_capture_dedup_mutation_deadline(mutation_deadline_millis: Option<i64>) -> Result<()> {
    if let Some(deadline_millis) = mutation_deadline_millis {
        ensure_scope_binding_mutation_deadline(deadline_millis)?;
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_capture_dedup_mutation_deadline_io(
    mutation_deadline_millis: Option<i64>,
) -> std::io::Result<()> {
    ensure_capture_dedup_mutation_deadline(mutation_deadline_millis)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::TimedOut, format!("{error:#}")))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn pending_alias_domain_is_distinct_and_readonly() {
        let root = tempfile::tempdir().expect("isolated alias key fixture");
        let storage = root.path().join("storage");
        fs::create_dir(&storage).expect("create isolated storage");
        let deadline = Instant::now() + Duration::from_secs(5);
        let body = b"canonical private association";
        assert!(
            authenticate_private_artifact_existing_key(
                &storage,
                body,
                None,
                deadline,
                PrivateArtifactMacDomain::SessionAlias,
            )
            .is_err()
        );
        assert_eq!(fs::read_dir(&storage).unwrap().count(), 0);
        let original = load_capture_dedup_secret(&storage).expect("initialize ingress key");
        let alias = authenticate_private_artifact_existing_key(
            &storage,
            body,
            None,
            deadline,
            PrivateArtifactMacDomain::SessionAlias,
        )
        .expect("sign association");
        assert!(alias.starts_with("pending-alias/hmac-v1/"));
        assert_eq!(
            authenticate_private_artifact_existing_key(
                &storage,
                body,
                Some(&alias),
                deadline,
                PrivateArtifactMacDomain::SessionAlias,
            )
            .unwrap(),
            alias
        );
        let envelope =
            authenticate_pending_envelope_existing_key(&storage, body, None, deadline).unwrap();
        assert_ne!(alias.rsplit('/').next(), envelope.rsplit('/').next());
        assert!(
            authenticate_pending_envelope_existing_key(&storage, body, Some(&alias), deadline,)
                .is_err()
        );
        assert!(
            authenticate_private_artifact_existing_key(
                &storage,
                body,
                Some(&envelope),
                deadline,
                PrivateArtifactMacDomain::SessionAlias,
            )
            .is_err()
        );
        assert!(
            authenticate_private_artifact_existing_key(
                &storage,
                b"changed association",
                Some(&alias),
                deadline,
                PrivateArtifactMacDomain::SessionAlias,
            )
            .is_err()
        );
        assert!(
            authenticate_private_artifact_existing_key(
                &storage,
                &vec![b'x'; 8193],
                None,
                deadline,
                PrivateArtifactMacDomain::SessionAlias,
            )
            .is_err()
        );
        let private = storage.join(CAPTURE_DEDUP_SECRET_DIR);
        let key_path = private.join(CAPTURE_DEDUP_SECRET_FILE);
        assert_eq!(fs::read(&key_path).unwrap(), original);
        fs::remove_file(&key_path).expect("simulate explicit key loss");
        let entries_before = fs::read_dir(&private).unwrap().count();
        assert!(
            authenticate_private_artifact_existing_key(
                &storage,
                body,
                Some(&alias),
                deadline,
                PrivateArtifactMacDomain::SessionAlias,
            )
            .is_err()
        );
        assert!(!key_path.exists());
        assert_eq!(fs::read_dir(&private).unwrap().count(), entries_before);
    }

    #[cfg(unix)]
    #[test]
    fn pending_verification_does_not_create_missing_key() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("create missing-key fixture");
        let storage = root.path().join("storage");
        fs::create_dir(&storage).expect("create storage");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mac = format!("{PENDING_ENVELOPE_MAC_PREFIX}{}", "0".repeat(64));
        assert!(
            authenticate_pending_envelope_existing_key(
                &storage,
                b"redacted fixture",
                Some(&mac),
                deadline,
            )
            .is_err()
        );
        assert_eq!(fs::read_dir(&storage).expect("inspect storage").count(), 0);

        let private = storage.join(CAPTURE_DEDUP_SECRET_DIR);
        fs::create_dir(&private).expect("create private directory");
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).expect("secure directory");
        assert!(
            authenticate_pending_envelope_existing_key(
                &storage,
                b"redacted fixture",
                Some(&mac),
                deadline,
            )
            .is_err()
        );
        assert_eq!(
            fs::read_dir(&private)
                .expect("inspect private directory")
                .count(),
            0
        );

        fs::set_permissions(&private, fs::Permissions::from_mode(0o755))
            .expect("broaden directory");
        assert!(
            authenticate_pending_envelope_existing_key(
                &storage,
                b"redacted fixture",
                Some(&mac),
                deadline,
            )
            .is_err()
        );
        assert_eq!(
            fs::metadata(&private)
                .expect("inspect unchanged mode")
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            fs::read_dir(&private)
                .expect("inspect unchanged entries")
                .count(),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn pending_mac_binds_all_bytes_and_preserves_existing_key() {
        let root = tempfile::tempdir().expect("create MAC fixture");
        let storage = root.path().join("storage");
        fs::create_dir(&storage).expect("create storage");
        let original = load_capture_dedup_secret(&storage).expect("initialize trusted key");
        let deadline = Instant::now() + Duration::from_secs(5);
        let envelope = b"redacted checkpoint envelope";
        let mac = authenticate_pending_envelope_existing_key(&storage, envelope, None, deadline)
            .expect("authenticate sealed envelope");
        assert_eq!(
            authenticate_pending_envelope_existing_key(&storage, envelope, Some(&mac), deadline,)
                .expect("verify exact envelope"),
            mac
        );
        assert!(
            authenticate_pending_envelope_existing_key(
                &storage,
                b"changed checkpoint envelope",
                Some(&mac),
                deadline,
            )
            .is_err()
        );
        assert!(
            authenticate_pending_envelope_existing_key(
                &storage,
                envelope,
                Some("source/hmac-v2/not-a-pending-tag"),
                deadline,
            )
            .is_err()
        );
        assert!(
            authenticate_pending_envelope_existing_key(
                &storage,
                envelope,
                Some(&mac),
                Instant::now(),
            )
            .is_err()
        );
        assert_eq!(
            load_capture_dedup_secret(&storage).expect("reopen established key"),
            original
        );
        assert_eq!(
            fs::read(storage.join("private/agent-capture-dedup-v1.key"))
                .expect("read unchanged key location"),
            original
        );
        let key = hmac::Key::new(hmac::HMAC_SHA256, &original);
        let digest = ring::digest::digest(&ring::digest::SHA256, envelope);
        for domain in [
            CaptureSourceCommitmentDomain::SnapshotContentV2,
            CaptureSourceCommitmentDomain::ImportSourceV2,
            CaptureSourceCommitmentDomain::SubagentSourceV2,
        ] {
            let mut context = hmac::Context::with_key(&key);
            context.update(domain.tag());
            context.update(digest.as_ref());
            assert_ne!(
                mac.strip_prefix(PENDING_ENVELOPE_MAC_PREFIX)
                    .expect("pending prefix"),
                hex::encode(context.sign().as_ref())
            );
        }
        assert_eq!(
            CaptureSourceCommitmentDomain::SnapshotContentV2.tag(),
            b"libra-agent-snapshot-content-hmac-v2\0"
        );
        assert_eq!(
            CaptureSourceCommitmentDomain::ImportSourceV2.tag(),
            b"libra-agent-import-source-hmac-v2\0"
        );
        assert_eq!(
            CaptureSourceCommitmentDomain::SubagentSourceV2.tag(),
            b"libra-subagent-source-hmac-v2\0"
        );
    }

    #[cfg(unix)]
    #[test]
    fn pending_verification_rejects_symlinks_and_preserves_targets() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("create symlink fixture");
        let storage = root.path().join("storage");
        let outside = root.path().join("outside");
        fs::create_dir(&storage).expect("create storage");
        fs::create_dir(&outside).expect("create target");
        symlink(&outside, storage.join("private")).expect("plant directory symlink");
        assert!(
            authenticate_pending_envelope_existing_key(
                &storage,
                b"redacted fixture",
                None,
                Instant::now() + Duration::from_secs(5),
            )
            .is_err()
        );
        assert_eq!(fs::read_dir(&outside).expect("inspect target").count(), 0);
    }

    const ALL_SOURCE_COMMITMENT_DOMAINS: [CaptureSourceCommitmentDomain; 3] = [
        CaptureSourceCommitmentDomain::SnapshotContentV2,
        CaptureSourceCommitmentDomain::ImportSourceV2,
        CaptureSourceCommitmentDomain::SubagentSourceV2,
    ];
    const KEY_BINDING_BODY: &[u8] = b"redacted key-binding fixture";
    const LIVE_LEASE_EXPIRES_AT: i64 = 9_999_999_999_999;

    fn key_binding_deadline() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    /// A real main-worktree repository layout (storage, SQLite schema and
    /// `libra.repoid`) built without changing the process cwd, so the scope
    /// façade resolves and canonicalizes exactly as production callers do.
    struct KeyBindingRepo {
        root: tempfile::TempDir,
        storage: PathBuf,
        conn: sea_orm::DatabaseConnection,
    }

    impl KeyBindingRepo {
        async fn new(repo_id: &str) -> Self {
            let root = tempfile::tempdir().expect("create key-binding repository");
            let storage = root.path().join(crate::utils::util::ROOT_DIR);
            fs::create_dir_all(storage.join("objects")).expect("create key-binding storage");
            let database = storage.join(crate::utils::util::DATABASE);
            let conn = crate::internal::db::create_database(
                database.to_str().expect("UTF-8 key-binding database path"),
            )
            .await
            .expect("create key-binding database");
            crate::internal::config::ConfigKv::set_with_conn(&conn, "libra.repoid", repo_id, false)
                .await
                .expect("seed key-binding repository identity");
            Self {
                root,
                storage,
                conn,
            }
        }

        fn root(&self) -> &Path {
            self.root.path()
        }

        async fn scope(&self) -> CaptureScope {
            CaptureScope::resolve(&self.conn, self.root())
                .await
                .expect("resolve key-binding scope")
        }

        /// Strings that a content-free rejection must never echo.
        fn identifying_strings(&self) -> Vec<String> {
            let canonical = fs::canonicalize(self.root()).expect("canonicalize repository root");
            vec![
                self.root().display().to_string(),
                canonical.display().to_string(),
                self.storage.display().to_string(),
            ]
        }
    }

    async fn set_workspace_lease_expiry(conn: &sea_orm::DatabaseConnection, expires_at: i64) {
        use sea_orm::ConnectionTrait;

        let result = conn
            .execute_raw(sea_orm::Statement::from_sql_and_values(
                conn.get_database_backend(),
                "UPDATE workspace_record SET lease_expires_at = ? WHERE workspace_id = ?",
                [expires_at.into(), "acf14-lease-workspace".into()],
            ))
            .await
            .expect("update key-binding workspace lease");
        assert_eq!(result.rows_affected(), 1, "lease fixture row must exist");
    }

    /// Everything a key-guarded call could create or rewrite: the private
    /// namespace listing and mode, plus the key bytes and inode times.
    #[derive(Debug, PartialEq, Eq)]
    struct KeyNamespaceState {
        entries: Vec<std::ffi::OsString>,
        private_mode: u32,
        key: Vec<u8>,
        key_inode: u64,
        key_mode: u32,
        key_modified: (i64, i64),
        key_changed: (i64, i64),
    }

    fn key_namespace_state(storage: &Path) -> Option<KeyNamespaceState> {
        let private = storage.join(CAPTURE_DEDUP_SECRET_DIR);
        let private_metadata = match fs::symlink_metadata(&private) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => panic!("inspect private key namespace: {error}"),
        };
        let mut entries = fs::read_dir(&private)
            .expect("list private key namespace")
            .map(|entry| entry.expect("read private key entry").file_name())
            .collect::<Vec<_>>();
        entries.sort();
        let key_path = private.join(CAPTURE_DEDUP_SECRET_FILE);
        let key_metadata = fs::symlink_metadata(&key_path).expect("inspect private key");
        Some(KeyNamespaceState {
            entries,
            private_mode: private_metadata.mode(),
            key: fs::read(&key_path).expect("read private key"),
            key_inode: key_metadata.ino(),
            key_mode: key_metadata.mode(),
            key_modified: (key_metadata.mtime(), key_metadata.mtime_nsec()),
            key_changed: (key_metadata.ctime(), key_metadata.ctime_nsec()),
        })
    }

    /// Positive control: the exact call set must succeed for a live binding,
    /// so the rejection assertions below cannot pass on a broken fixture.
    async fn mint_with_live_key_binding(
        conn: &sea_orm::DatabaseConnection,
        scope: &CaptureScope,
        storage: &Path,
        root: &Path,
    ) -> String {
        for domain in ALL_SOURCE_COMMITMENT_DOMAINS {
            derive_capture_source_commitment_in_scope_until(
                conn,
                scope,
                storage,
                root,
                domain,
                &[0x5A; 32],
                key_binding_deadline(),
            )
            .await
            .expect("a live key binding derives its source commitment");
        }
        let mac = scope
            .sign_pending_envelope_until(
                conn,
                storage,
                root,
                KEY_BINDING_BODY,
                key_binding_deadline(),
            )
            .await
            .expect("a live key binding signs its pending envelope");
        scope
            .verify_pending_envelope_until(
                conn,
                storage,
                root,
                KEY_BINDING_BODY,
                &mac,
                key_binding_deadline(),
            )
            .await
            .expect("a live key binding verifies its pending envelope");
        authenticate_pending_alias_in_scope_until(
            conn,
            scope,
            storage,
            root,
            KEY_BINDING_BODY,
            None,
            key_binding_deadline(),
        )
        .await
        .expect("a live key binding authenticates its pending alias");
        mac
    }

    fn assert_content_free_rejection(error: anyhow::Error, expected: &str, forbidden: &[String]) {
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(expected),
            "rejection must report the fixed key-binding diagnostic {expected:?}: {rendered}"
        );
        for value in forbidden {
            assert!(
                !rendered.contains(value.as_str()),
                "key-binding rejection must not echo repository paths or ids: {rendered}"
            );
        }
    }

    /// Drive every key-guarded entry point (all source-commitment domains,
    /// envelope sign/verify, alias MAC) through one rejected binding.
    async fn assert_key_binding_rejected(
        conn: &sea_orm::DatabaseConnection,
        scope: &CaptureScope,
        storage: &Path,
        root: &Path,
        mac: Option<&str>,
        expected: &str,
        forbidden: &[String],
    ) {
        for domain in ALL_SOURCE_COMMITMENT_DOMAINS {
            let error = derive_capture_source_commitment_in_scope_until(
                conn,
                scope,
                storage,
                root,
                domain,
                &[0x5A; 32],
                key_binding_deadline(),
            )
            .await
            .expect_err("a rejected key binding must not derive a source commitment");
            assert_content_free_rejection(error, expected, forbidden);
        }
        let error = scope
            .sign_pending_envelope_until(
                conn,
                storage,
                root,
                KEY_BINDING_BODY,
                key_binding_deadline(),
            )
            .await
            .expect_err("a rejected key binding must not sign a pending envelope");
        assert_content_free_rejection(error, expected, forbidden);
        let error = authenticate_pending_alias_in_scope_until(
            conn,
            scope,
            storage,
            root,
            KEY_BINDING_BODY,
            None,
            key_binding_deadline(),
        )
        .await
        .expect_err("a rejected key binding must not authenticate a pending alias");
        assert_content_free_rejection(error, expected, forbidden);
        let unverifiable = format!("{PENDING_ENVELOPE_MAC_PREFIX}{}", "0".repeat(64));
        let error = scope
            .verify_pending_envelope_until(
                conn,
                storage,
                root,
                KEY_BINDING_BODY,
                mac.unwrap_or(&unverifiable),
                key_binding_deadline(),
            )
            .await
            .expect_err("a rejected key binding must not verify a pending envelope");
        assert_content_free_rejection(error, expected, forbidden);
    }

    #[tokio::test]
    async fn key_binding_rejects_a_dead_workspace_lease_before_key_io() {
        use sea_orm::ConnectionTrait;

        const LEASE_REJECTED: &str =
            "capture workspace lease is no longer live at its recorded fence";

        let repo = KeyBindingRepo::new("acf14-lease-repo").await;
        let canonical_root = fs::canonicalize(repo.root()).expect("canonicalize lease root");
        repo.conn
            .execute_raw(sea_orm::Statement::from_sql_and_values(
                repo.conn.get_database_backend(),
                "INSERT INTO workspace_record (
                     workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                     state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
                 ) VALUES ('acf14-lease-workspace', 'acf14-lease-repo', 'task_copy', NULL, ?,
                           'agent', 'active', 'acf14-lease-owner', 11, ?, 1, 1)",
                [
                    canonical_root
                        .to_str()
                        .expect("UTF-8 lease root")
                        .to_string()
                        .into(),
                    LIVE_LEASE_EXPIRES_AT.into(),
                ],
            ))
            .await
            .expect("seed live key-binding workspace");
        let scope = repo.scope().await;
        assert_eq!(
            (scope.workspace_id.as_deref(), scope.workspace_fence),
            (Some("acf14-lease-workspace"), Some(11)),
            "fixture must resolve a leased workspace scope"
        );
        let mut forbidden = repo.identifying_strings();
        forbidden.extend(
            [
                "acf14-lease-repo",
                "acf14-lease-workspace",
                "acf14-lease-owner",
            ]
            .map(String::from),
        );

        // The resolved scope still equals the caller's (only the lease
        // expired), so this reaches the live-lease branch. With no key on
        // disk, a bypassed guard would mint one.
        set_workspace_lease_expiry(&repo.conn, 0).await;
        assert_key_binding_rejected(
            &repo.conn,
            &scope,
            &repo.storage,
            repo.root(),
            None,
            LEASE_REJECTED,
            &forbidden,
        )
        .await;
        let error = derive_capture_source_commitment_in_scope_until(
            &repo.conn,
            &scope,
            &repo.storage,
            repo.root(),
            CaptureSourceCommitmentDomain::ImportSourceV2,
            &[0x5A; 32],
            key_binding_deadline(),
        )
        .await
        .expect_err("a dead lease must not derive a source commitment");
        assert!(
            error.chain().any(|cause| matches!(
                cause.downcast_ref::<crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError>(),
                Some(crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError::WorkspaceFenceRejected)
            )),
            "a dead lease must keep its typed fence rejection: {error:#}"
        );
        assert_eq!(
            key_namespace_state(&repo.storage),
            None,
            "a dead workspace lease must not create the private key namespace"
        );

        set_workspace_lease_expiry(&repo.conn, LIVE_LEASE_EXPIRES_AT).await;
        let mac = mint_with_live_key_binding(&repo.conn, &scope, &repo.storage, repo.root()).await;
        let established =
            key_namespace_state(&repo.storage).expect("live binding initialized the key");

        set_workspace_lease_expiry(&repo.conn, 0).await;
        assert_key_binding_rejected(
            &repo.conn,
            &scope,
            &repo.storage,
            repo.root(),
            Some(&mac),
            LEASE_REJECTED,
            &forbidden,
        )
        .await;
        assert_eq!(
            key_namespace_state(&repo.storage).as_ref(),
            Some(&established),
            "a dead workspace lease must leave the existing key untouched"
        );

        // A fenced-out holder of the same live workspace is a different scope.
        set_workspace_lease_expiry(&repo.conn, LIVE_LEASE_EXPIRES_AT).await;
        let mut stale = scope.clone();
        stale.workspace_fence = Some(10);
        assert_key_binding_rejected(
            &repo.conn,
            &stale,
            &repo.storage,
            repo.root(),
            Some(&mac),
            "capture commitment root does not belong to the authorized scope",
            &forbidden,
        )
        .await;
        assert_eq!(
            key_namespace_state(&repo.storage),
            Some(established),
            "a stale workspace fence must leave the existing key untouched"
        );
    }

    #[tokio::test]
    async fn key_binding_rejects_sibling_storage_before_key_io() {
        const STORAGE_REJECTED: &str =
            "capture commitment storage does not belong to the authorized repository root";

        let authorized = KeyBindingRepo::new("acf14-authorized-repo").await;
        let sibling = KeyBindingRepo::new("acf14-sibling-repo").await;
        let scope = authorized.scope().await;
        let mut forbidden = authorized.identifying_strings();
        forbidden.extend(sibling.identifying_strings());
        forbidden.extend(["acf14-authorized-repo", "acf14-sibling-repo"].map(String::from));

        // The authorized scope and root are genuine; only the storage belongs
        // to a sibling. A bypassed guard would mint the sibling's key.
        assert_key_binding_rejected(
            &authorized.conn,
            &scope,
            &sibling.storage,
            authorized.root(),
            None,
            STORAGE_REJECTED,
            &forbidden,
        )
        .await;
        assert_eq!(key_namespace_state(&sibling.storage), None);
        assert_eq!(key_namespace_state(&authorized.storage), None);

        // Give the sibling a real key and a MAC that would verify under it.
        let sibling_scope = sibling.scope().await;
        let sibling_mac = mint_with_live_key_binding(
            &sibling.conn,
            &sibling_scope,
            &sibling.storage,
            sibling.root(),
        )
        .await;
        let established =
            key_namespace_state(&sibling.storage).expect("sibling binding initialized its key");
        assert_key_binding_rejected(
            &authorized.conn,
            &scope,
            &sibling.storage,
            authorized.root(),
            Some(&sibling_mac),
            STORAGE_REJECTED,
            &forbidden,
        )
        .await;
        assert_eq!(
            key_namespace_state(&sibling.storage),
            Some(established),
            "sibling storage must stay untouched by another repository's scope"
        );
        assert_eq!(
            key_namespace_state(&authorized.storage),
            None,
            "a rejected sibling binding must not create the authorized repository key"
        );
    }

    #[tokio::test]
    async fn key_binding_rejects_a_scope_from_another_repository_before_key_io() {
        const SCOPE_REJECTED: &str =
            "capture commitment root does not belong to the authorized scope";

        let authorized = KeyBindingRepo::new("acf14-scope-owner-repo").await;
        let foreign = KeyBindingRepo::new("acf14-foreign-scope-repo").await;
        let foreign_scope = foreign.scope().await;
        let mut forbidden = authorized.identifying_strings();
        forbidden.extend(foreign.identifying_strings());
        forbidden.extend(["acf14-scope-owner-repo", "acf14-foreign-scope-repo"].map(String::from));

        // Storage and root agree with each other, but the scope was resolved
        // in another repository. A bypassed guard would mint this repo's key.
        assert_key_binding_rejected(
            &authorized.conn,
            &foreign_scope,
            &authorized.storage,
            authorized.root(),
            None,
            SCOPE_REJECTED,
            &forbidden,
        )
        .await;
        assert_eq!(key_namespace_state(&authorized.storage), None);
        assert_eq!(key_namespace_state(&foreign.storage), None);

        let scope = authorized.scope().await;
        assert_ne!(scope, foreign_scope);
        let mac = mint_with_live_key_binding(
            &authorized.conn,
            &scope,
            &authorized.storage,
            authorized.root(),
        )
        .await;
        let established =
            key_namespace_state(&authorized.storage).expect("authorized binding initialized key");
        assert_key_binding_rejected(
            &authorized.conn,
            &foreign_scope,
            &authorized.storage,
            authorized.root(),
            Some(&mac),
            SCOPE_REJECTED,
            &forbidden,
        )
        .await;
        assert_eq!(
            key_namespace_state(&authorized.storage),
            Some(established),
            "a foreign scope must leave the existing key untouched"
        );
        assert_eq!(key_namespace_state(&foreign.storage), None);
    }

    /// Byte-compatibility vectors for every keyed capture domain. The expected
    /// strings were computed outside Rust (Python `hmac`/`hashlib`, and
    /// cross-checked with `openssl dgst -sha256 -mac HMAC`) from the documented
    /// compositions: `prefix || lowercase_hex(HMAC-SHA256(key, tag || preimage))`
    /// for source commitments and
    /// `prefix || lowercase_hex(HMAC-SHA256(key, tag || SHA-256(body)))` for
    /// pending envelopes/aliases. Never regenerate them from this code.
    #[tokio::test]
    async fn keyed_capture_domains_match_independent_known_answer_vectors() {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        let repo = KeyBindingRepo::new("acf14-known-answer-repo").await;
        let key: [u8; CAPTURE_DEDUP_SECRET_BYTES] = std::array::from_fn(|index| index as u8);
        let preimage: [u8; 32] = std::array::from_fn(|index| 0x80 + index as u8);
        let body = b"libra ACF-14 known-answer redacted envelope\n";

        // Plant the key exactly where earlier releases stored it, so the
        // vectors also prove the loader reuses (never rotates) that file.
        let private = repo.storage.join(CAPTURE_DEDUP_SECRET_DIR);
        fs::create_dir(&private).expect("create planted private directory");
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700))
            .expect("secure planted private directory");
        let key_path = private.join(CAPTURE_DEDUP_SECRET_FILE);
        let mut key_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&key_path)
            .expect("create planted key");
        key_file.write_all(&key).expect("write planted key");
        key_file.sync_all().expect("flush planted key");
        drop(key_file);
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))
            .expect("secure planted key");

        let scope = repo.scope().await;
        for (domain, expected) in [
            (
                CaptureSourceCommitmentDomain::SnapshotContentV2,
                "source/hmac-v2/2d3819b84be90184f4716d94388583100e8c8f8c3ec73d22bcc295ab2516d4d6",
            ),
            (
                CaptureSourceCommitmentDomain::ImportSourceV2,
                "source/hmac-v2/641f9aceff9f0fbf8bbe513f59a1f017b50990d5fc752670be8f5b36a22c1f22",
            ),
            (
                CaptureSourceCommitmentDomain::SubagentSourceV2,
                "source/subagent-hmac-v2/b59fc8c555b00139003e454463d5d0be8a9e70477cd07ef79cafc1c08af319a8",
            ),
        ] {
            assert_eq!(
                derive_capture_source_commitment_in_scope_until(
                    &repo.conn,
                    &scope,
                    &repo.storage,
                    repo.root(),
                    domain,
                    &preimage,
                    key_binding_deadline(),
                )
                .await
                .expect("derive known-answer source commitment"),
                expected,
                "{domain:?} source commitment output changed"
            );
        }
        assert_eq!(
            derive_snapshot_content_commitment_in_scope_until(
                &repo.conn,
                &scope,
                &repo.storage,
                repo.root(),
                &preimage,
                key_binding_deadline(),
            )
            .await
            .expect("derive known-answer snapshot commitment"),
            "source/hmac-v2/2d3819b84be90184f4716d94388583100e8c8f8c3ec73d22bcc295ab2516d4d6",
            "the snapshot wrapper must keep the SnapshotContentV2 domain"
        );

        const ENVELOPE_MAC: &str = "pending-envelope/hmac-v1/2e483f6b69f3839f7069aec7ecad84044745fb638ad717fbf8a5fd84ebe26781";
        assert_eq!(
            scope
                .sign_pending_envelope_until(
                    &repo.conn,
                    &repo.storage,
                    repo.root(),
                    body,
                    key_binding_deadline(),
                )
                .await
                .expect("sign known-answer envelope"),
            ENVELOPE_MAC
        );
        scope
            .verify_pending_envelope_until(
                &repo.conn,
                &repo.storage,
                repo.root(),
                body,
                ENVELOPE_MAC,
                key_binding_deadline(),
            )
            .await
            .expect("verify independently computed envelope MAC");
        const ALIAS_MAC: &str = "pending-alias/hmac-v1/59804f767e0aa7b9e8c9126757290a71e7ae956c3f97d835159858f8e9c0ef67";
        assert_eq!(
            authenticate_pending_alias_in_scope_until(
                &repo.conn,
                &scope,
                &repo.storage,
                repo.root(),
                body,
                Some(ALIAS_MAC),
                key_binding_deadline(),
            )
            .await
            .expect("verify independently computed alias MAC"),
            ALIAS_MAC
        );
        assert_eq!(
            fs::read(&key_path).expect("reread planted key"),
            key,
            "known-answer derivation must reuse the existing key bytes"
        );
    }

    // These names are read only by this cfg(test) re-exec fixture. They are
    // never consulted by the production helper or its environment contract.
    #[cfg(unix)]
    const CAPTURE_DEDUP_KILL_CHILD_MODE_ENV: &str = "LIBRA_TEST_CAPTURE_DEDUP_KILL_CHILD";
    #[cfg(unix)]
    const CAPTURE_DEDUP_KILL_CHILD_STORAGE_ENV: &str = "LIBRA_TEST_CAPTURE_DEDUP_KILL_STORAGE";
    #[cfg(unix)]
    const CAPTURE_DEDUP_KILL_CHILD_READY_ENV: &str = "LIBRA_TEST_CAPTURE_DEDUP_KILL_READY";
    #[cfg(unix)]
    const CAPTURE_DEDUP_KILL_CHILD_TEST_NAME: &str = "internal::ai::capture::key::tests::capture_dedup_key_recovers_after_sigkill_of_actual_staging_child";

    /// Run the child side of the real-SIGKILL regression. This is a re-exec of
    /// the cfg(test) unit binary, not a production hook interface: it creates
    /// and locks a real staging file through the shipping helpers, signals
    /// readiness, and then deliberately waits for the parent to kill it.
    #[cfg(unix)]
    fn run_capture_dedup_kill_child() -> ! {
        use std::{io::Write, path::PathBuf, time::Duration};

        let storage = PathBuf::from(
            std::env::var_os(CAPTURE_DEDUP_KILL_CHILD_STORAGE_ENV)
                .expect("child must receive its isolated storage path"),
        );
        let ready = PathBuf::from(
            std::env::var_os(CAPTURE_DEDUP_KILL_CHILD_READY_ENV)
                .expect("child must receive its isolated ready path"),
        );
        let private = CaptureDedupPrivateDir::open_or_create(
            &storage.join(CAPTURE_DEDUP_SECRET_DIR),
            "key directory",
            None,
        )
        .expect("child creates private key directory");
        let staging = private
            .open_or_create_child(CAPTURE_DEDUP_TEMP_DIR, "staging directory", None)
            .expect("child creates key staging directory");
        let staging_lock = staging
            .try_acquire_staging_lock(None)
            .expect("child opens secure staging lock")
            .expect("child owns staging lock");
        let mut temporary = staging
            .create_temp_file(&staging_lock, None)
            .expect("child creates and locks real staging file");
        temporary
            .file
            .write_all(&[42_u8; CAPTURE_DEDUP_SECRET_BYTES])
            .expect("child writes staging key bytes");
        temporary
            .file
            .sync_all()
            .expect("child flushes staging key bytes");
        std::fs::write(&ready, b"ready").expect("child reports staging readiness");

        // Keep the descriptor (and its flock) alive until SIGKILL. A normal
        // return would run Drop and would not exercise crash-residue cleanup.
        let _keep_staging_lock = staging_lock;
        let _keep_staging_file = temporary;
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }

    /// A scope-binding helper may be SIGKILLed after it creates its key
    /// staging file but before Rust can run its Drop cleanup. The next helper
    /// must reap that released artifact without treating arbitrary siblings,
    /// unsafe file types, weakly-permissioned files, or a live writer as ours.
    #[cfg(unix)]
    #[test]
    fn capture_dedup_key_reclaims_only_released_attributable_staging_files() {
        use std::{
            fs::OpenOptions,
            io::Write,
            os::{
                fd::AsRawFd,
                unix::fs::{OpenOptionsExt, PermissionsExt, symlink},
            },
        };

        let root = tempfile::tempdir().expect("create capture-key tempdir");
        let storage = root.path().join("storage");
        std::fs::create_dir(&storage).expect("create storage");
        let initial = load_capture_dedup_secret(&storage).expect("create initial capture key");
        let staging = storage
            .join(CAPTURE_DEDUP_SECRET_DIR)
            .join(CAPTURE_DEDUP_TEMP_DIR);

        let released = staging.join(format!(
            "{CAPTURE_DEDUP_TEMP_PREFIX}{}",
            "a".repeat(CAPTURE_DEDUP_TEMP_RANDOM_HEX_BYTES * 2)
        ));
        std::fs::write(&released, [7_u8; CAPTURE_DEDUP_SECRET_BYTES])
            .expect("write released staging artifact");
        std::fs::set_permissions(&released, std::fs::Permissions::from_mode(0o600))
            .expect("secure released staging artifact");
        let partial = staging.join(format!(
            "{CAPTURE_DEDUP_TEMP_PREFIX}{}",
            "e".repeat(CAPTURE_DEDUP_TEMP_RANDOM_HEX_BYTES * 2)
        ));
        std::fs::write(&partial, b"").expect("write partially-created staging artifact");
        std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o600))
            .expect("secure partially-created staging artifact");
        let published_residue = staging.join(format!(
            "{CAPTURE_DEDUP_TEMP_PREFIX}{}",
            "f".repeat(CAPTURE_DEDUP_TEMP_RANDOM_HEX_BYTES * 2)
        ));
        std::fs::hard_link(
            storage
                .join(CAPTURE_DEDUP_SECRET_DIR)
                .join(CAPTURE_DEDUP_SECRET_FILE),
            &published_residue,
        )
        .expect("model helper killed after linkat but before staging unlink");

        let foreign = staging.join("notes-from-user.tmp");
        std::fs::write(&foreign, b"do not remove").expect("write unrelated staging sibling");

        let outside = root.path().join("outside-sentinel");
        std::fs::write(&outside, b"untouched").expect("write outside sentinel");
        let unsafe_link = staging.join(format!(
            "{CAPTURE_DEDUP_TEMP_PREFIX}{}",
            "b".repeat(CAPTURE_DEDUP_TEMP_RANDOM_HEX_BYTES * 2)
        ));
        symlink(&outside, &unsafe_link).expect("plant staging symlink");

        let weak_mode = staging.join(format!(
            "{CAPTURE_DEDUP_TEMP_PREFIX}{}",
            "c".repeat(CAPTURE_DEDUP_TEMP_RANDOM_HEX_BYTES * 2)
        ));
        std::fs::write(&weak_mode, [8_u8; CAPTURE_DEDUP_SECRET_BYTES])
            .expect("write weak-mode staging file");
        std::fs::set_permissions(&weak_mode, std::fs::Permissions::from_mode(0o644))
            .expect("weaken staging mode");

        let live = staging.join(format!(
            "{CAPTURE_DEDUP_TEMP_PREFIX}{}",
            "d".repeat(CAPTURE_DEDUP_TEMP_RANDOM_HEX_BYTES * 2)
        ));
        let mut live_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&live)
            .expect("create live staging file");
        live_file
            .write_all(&[9_u8; CAPTURE_DEDUP_SECRET_BYTES])
            .expect("write live staging file");
        // SAFETY: the test holds `live_file` until after the first cleanup.
        assert_eq!(
            unsafe { libc::flock(live_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0,
            "hold the live staging lock"
        );

        assert_eq!(
            load_capture_dedup_secret(&storage).expect("reopen capture key"),
            initial,
            "cleanup must preserve the established replay identity"
        );
        assert!(
            !released.exists(),
            "a released valid staging artifact models the SIGKILL residue and must be reclaimed"
        );
        assert!(
            !partial.exists(),
            "a SIGKILL during the write leaves a short artifact that must be reclaimed too"
        );
        assert!(
            !published_residue.exists(),
            "a SIGKILL after linkat must reclaim only the staging hard link and retain the final key"
        );
        assert!(foreign.exists(), "unknown user sibling must be preserved");
        assert!(
            std::fs::symlink_metadata(&unsafe_link)
                .expect("inspect unsafe staging link")
                .file_type()
                .is_symlink(),
            "a matching symlink must not be unlinked"
        );
        assert_eq!(
            std::fs::read(&outside).expect("read outside sentinel"),
            b"untouched",
            "cleanup must never follow a staged symlink"
        );
        assert!(
            weak_mode.exists(),
            "a matching file without our strict 0600 mode must be preserved"
        );
        assert!(
            live.exists(),
            "an active writer's locked staging file must survive"
        );

        drop(live_file);
        assert_eq!(
            load_capture_dedup_secret(&storage).expect("reopen after writer exits"),
            initial,
            "released live writer must not change the replay identity"
        );
        assert!(
            !live.exists(),
            "after process death releases flock, the next helper must reclaim the residue"
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_dedup_key_recovers_after_sigkill_of_actual_staging_child() {
        use std::{
            process::{Command, Stdio},
            time::{Duration, Instant},
        };

        if std::env::var_os(CAPTURE_DEDUP_KILL_CHILD_MODE_ENV).is_some() {
            run_capture_dedup_kill_child();
        }

        let root = tempfile::tempdir().expect("create SIGKILL staging tempdir");
        let storage = root.path().join("storage");
        let ready = root.path().join("child-staging-ready");
        std::fs::create_dir(&storage).expect("create isolated storage");
        let test_binary = std::env::current_exe().expect("locate current unit-test binary");
        let mut child = Command::new(test_binary)
            .args(["--exact", CAPTURE_DEDUP_KILL_CHILD_TEST_NAME, "--nocapture"])
            .env(CAPTURE_DEDUP_KILL_CHILD_MODE_ENV, "1")
            .env(CAPTURE_DEDUP_KILL_CHILD_STORAGE_ENV, &storage)
            .env(CAPTURE_DEDUP_KILL_CHILD_READY_ENV, &ready)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start isolated staging child");
        let ready_deadline = Instant::now() + Duration::from_secs(10);
        while !ready.is_file() && Instant::now() < ready_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !ready.is_file() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("SIGKILL fixture did not create and lock its staging file");
        }
        child.kill().expect("SIGKILL the staging child");
        let status = child.wait().expect("reap staging child");
        assert!(
            !status.success(),
            "the fixture must die without running its staging-file Drop cleanup"
        );

        let first = load_capture_dedup_secret(&storage)
            .expect("retry after SIGKILL must reclaim and publish a key");
        let second = load_capture_dedup_secret(&storage)
            .expect("a second retry must reuse the recovered key without new residue");
        assert_eq!(first, second, "retry must retain one stable replay key");
        let staging = storage
            .join(CAPTURE_DEDUP_SECRET_DIR)
            .join(CAPTURE_DEDUP_TEMP_DIR);
        let protocol_residue = std::fs::read_dir(&staging)
            .expect("enumerate staging after retry")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name())
            .filter(|name| capture_dedup_temp_name_is_valid(name))
            .count();
        assert_eq!(
            protocol_residue, 0,
            "the real killed-child residue must be reaped and retries must not accumulate staging files"
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_dedup_staging_lock_retries_only_bootstrap_enoent() {
        assert!(capture_dedup_staging_lock_open_is_bootstrap_transient(
            &std::io::Error::from_raw_os_error(libc::ENOENT)
        ));
        for errno in [libc::EACCES, libc::EINTR, libc::EIO, libc::ELOOP] {
            assert!(
                !capture_dedup_staging_lock_open_is_bootstrap_transient(
                    &std::io::Error::from_raw_os_error(errno)
                ),
                "only the APFS bootstrap ENOENT may enter the contention retry path (errno {errno})"
            );
        }
    }

    /// A creator owns the staging mutex before it creates the per-file flock.
    /// A second helper must not enter cleanup during that short interval: if
    /// it did, it could lock and unlink the first helper's just-created name
    /// before the creator gets a chance to lock its descriptor.
    #[cfg(unix)]
    #[test]
    fn capture_dedup_staging_mutex_hides_a_pre_file_lock_name_from_contenders() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("create staging-mutex tempdir");
        let storage = root.path().join("storage");
        std::fs::create_dir(&storage).expect("create storage");
        let private = CaptureDedupPrivateDir::open_or_create(
            &storage.join(CAPTURE_DEDUP_SECRET_DIR),
            "key directory",
            None,
        )
        .expect("create private directory");
        let staging = private
            .open_or_create_child(CAPTURE_DEDUP_TEMP_DIR, "staging directory", None)
            .expect("create staging directory");
        let staging_lock = staging
            .try_acquire_staging_lock(None)
            .expect("open staging mutex")
            .expect("own staging mutex");
        let pre_file_lock = staging.path.join(format!(
            "{CAPTURE_DEDUP_TEMP_PREFIX}{}",
            "a".repeat(CAPTURE_DEDUP_TEMP_RANDOM_HEX_BYTES * 2)
        ));
        std::fs::write(&pre_file_lock, [17_u8; CAPTURE_DEDUP_SECRET_BYTES])
            .expect("model creator after exclusive create but before per-file flock");
        std::fs::set_permissions(&pre_file_lock, std::fs::Permissions::from_mode(0o600))
            .expect("secure modeled pre-file-lock entry");

        let contender_storage = storage.clone();
        let deadline_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock follows Unix epoch")
            .as_millis()
            .checked_add(300)
            .and_then(|millis| i64::try_from(millis).ok())
            .expect("test deadline fits i64");
        let contender = std::thread::spawn(move || {
            load_capture_dedup_secret_with_mutation_deadline(
                &contender_storage,
                Some(deadline_millis),
            )
        })
        .join()
        .expect("contender must not panic");
        let error = contender.expect_err("a bounded contender must not bypass the staging mutex");
        assert!(
            format!("{error:#}").contains("deadline elapsed"),
            "a contender must wait/read final through its host deadline instead of sweeping an unlocked creator entry: {error:#}"
        );
        assert!(
            pre_file_lock.exists(),
            "the staging mutex must keep the creator's pre-file-lock name invisible to cleanup"
        );

        drop(staging_lock);
        load_capture_dedup_secret(&storage)
            .expect("the next mutex owner must reclaim the now-released modeled residue");
        assert!(
            !pre_file_lock.exists(),
            "once the creator no longer owns the mutex, ordinary conservative cleanup may reclaim it"
        );
    }

    /// The scoped helper's wall-clock deadline is a second mutation gate, not
    /// merely a preflight check. Each namespace-creating or permission-changing
    /// syscall must refuse an already-expired deadline, and a staged key must
    /// not publish or clean itself after expiry.
    #[cfg(unix)]
    #[test]
    fn capture_dedup_expired_deadline_blocks_namespace_mutations() {
        use std::{io::Write, os::unix::fs::PermissionsExt};

        let root = tempfile::tempdir().expect("create deadline-gate tempdir");
        let storage = root.path().join("storage");
        let private_path = storage.join(CAPTURE_DEDUP_SECRET_DIR);
        std::fs::create_dir(&storage).expect("create storage");
        let expired = Some(0);

        assert!(
            CaptureDedupPrivateDir::open_or_create(&private_path, "key directory", expired)
                .is_err(),
            "an expired helper must not mkdir the private key namespace"
        );
        assert!(
            !private_path.exists(),
            "expired private-namespace creation must leave no directory"
        );

        let private = CaptureDedupPrivateDir::open_or_create(&private_path, "key directory", None)
            .expect("create private namespace without a deadline");
        assert!(
            private
                .open_or_create_child(CAPTURE_DEDUP_TEMP_DIR, "staging directory", expired)
                .is_err(),
            "an expired helper must not mkdirat the staging namespace"
        );
        let staging_path = private_path.join(CAPTURE_DEDUP_TEMP_DIR);
        assert!(
            !staging_path.exists(),
            "expired staging-namespace creation must leave no directory"
        );

        let staging = private
            .open_or_create_child(CAPTURE_DEDUP_TEMP_DIR, "staging directory", None)
            .expect("create staging namespace without a deadline");
        assert!(
            staging.try_acquire_staging_lock(expired).is_err(),
            "an expired helper must not create the staging lock"
        );
        assert!(
            !staging_path.join(CAPTURE_DEDUP_TEMP_LOCK_FILE).exists(),
            "expired lock creation must leave no lock entry"
        );

        std::fs::set_permissions(&private_path, std::fs::Permissions::from_mode(0o755))
            .expect("widen safe private directory for fchmod gate");
        assert!(
            CaptureDedupPrivateDir::open_path(&private_path, "key directory", expired).is_err(),
            "an expired helper must not fchmod an existing private directory"
        );
        assert_eq!(
            std::fs::metadata(&private_path)
                .expect("inspect private directory after expiry")
                .mode()
                & 0o777,
            0o755,
            "the expired fchmod gate must preserve the prior mode"
        );
        std::fs::set_permissions(&private_path, std::fs::Permissions::from_mode(0o700))
            .expect("restore private directory mode");

        let staging_lock = staging
            .try_acquire_staging_lock(None)
            .expect("open staging lock without a deadline")
            .expect("own staging lock");
        let mut temporary = staging
            .create_temp_file(&staging_lock, None)
            .expect("create staging key without a deadline");
        temporary
            .file
            .write_all(&[7_u8; CAPTURE_DEDUP_SECRET_BYTES])
            .expect("write staged key");
        temporary.file.sync_all().expect("flush staged key");
        let temporary_path = staging.path.join(temporary.name.to_string_lossy().as_ref());
        temporary.mutation_deadline_millis = expired;
        assert!(
            temporary.publish_noclobber(&private).is_err(),
            "an expired helper must not linkat-publish a staged key"
        );
        assert!(
            !private_path.join(CAPTURE_DEDUP_SECRET_FILE).exists(),
            "expired publication must not create the final key"
        );
        assert!(
            temporary_path.exists(),
            "expired Drop cleanup must preserve the attributable staging residue"
        );
        drop(temporary);
        assert!(
            temporary_path.exists(),
            "Drop must not unlink staging state after its mutation deadline"
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_dedup_private_dir_rejects_insecure_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("create insecure-private tempdir");
        let storage = root.path().join("storage");
        let private = storage.join(CAPTURE_DEDUP_SECRET_DIR);
        std::fs::create_dir(&storage).expect("create storage");
        std::fs::create_dir(&private).expect("create preexisting private directory");
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o777))
            .expect("make private directory group writable");

        let error = load_capture_dedup_secret(&storage)
            .expect_err("an already writable private directory is not safe to adopt");
        assert!(
            format!("{error:#}").contains("writable by group or other"),
            "failure must explain why the existing private directory was refused: {error:#}"
        );
        assert!(
            !private.join(CAPTURE_DEDUP_SECRET_FILE).exists(),
            "a rejected directory must not receive a final key"
        );
        assert!(
            !private.join(CAPTURE_DEDUP_TEMP_DIR).exists(),
            "a rejected directory must not receive a staging namespace"
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_dedup_private_dir_refuses_a_symlink_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("create private-symlink tempdir");
        let storage = root.path().join("storage");
        let outside = root.path().join("outside");
        std::fs::create_dir(&storage).expect("create storage");
        std::fs::create_dir(&outside).expect("create outside target");
        symlink(&outside, storage.join(CAPTURE_DEDUP_SECRET_DIR))
            .expect("plant private-directory symlink");

        assert!(
            load_capture_dedup_secret(&storage).is_err(),
            "a private-directory symlink must fail closed"
        );
        assert!(
            !outside.join(CAPTURE_DEDUP_SECRET_FILE).exists(),
            "a refused symlink must not create a key outside the repository"
        );
        assert!(
            !outside.join(CAPTURE_DEDUP_TEMP_DIR).exists(),
            "a refused symlink must not create a staging directory outside the repository"
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_dedup_private_dir_tightens_safe_legacy_permissions() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let root = tempfile::tempdir().expect("create legacy-private tempdir");
        let storage = root.path().join("storage");
        let private = storage.join(CAPTURE_DEDUP_SECRET_DIR);
        std::fs::create_dir(&storage).expect("create storage");
        std::fs::create_dir(&private).expect("create legacy private directory");
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o755))
            .expect("set safe-but-broad legacy private mode");

        load_capture_dedup_secret(&storage).expect("adopt safe legacy private directory");
        assert_eq!(
            std::fs::metadata(&private)
                .expect("inspect private directory")
                .mode()
                & 0o777,
            0o700,
            "a private directory that was never writable by other users is tightened in place"
        );
        assert_eq!(
            std::fs::metadata(private.join(CAPTURE_DEDUP_TEMP_DIR))
                .expect("inspect staging directory")
                .mode()
                & 0o777,
            0o700,
            "the dedicated staging namespace must be owner-only too"
        );
    }
}
