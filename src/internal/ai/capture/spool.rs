//! Filesystem queue for realtime AgentTraces hooks.
//!
//! A hook writes one already-redacted envelope under
//! `.libra/private/capture-spool/` and returns. The consumer in `drain`
//! is the only reader of `ready/`. This module never opens a database.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, anyhow};
use uuid::Uuid;

/// Hidden child argv. The parent recognizes it before ordinary startup.
pub(crate) const DRAIN_ARG: &str = "__capture-spool-drain";

const SPOOL_DIR: &str = "capture-spool";
const TMP_DIR: &str = "tmp";
const READY_DIR: &str = "ready";
const QUARANTINE_DIR: &str = "quarantine";
const CAPACITY_LOCK: &str = "capacity.lock";
const TMP_FILE_LIMIT: usize = 64;
const READY_FILE_LIMIT: usize = 1024;
const BYTE_LIMIT: u64 = 256 * 1024 * 1024;
const TMP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// The queue cannot accept another durable envelope.
#[derive(Debug)]
pub(crate) struct CaptureQueueFull;

impl std::fmt::Display for CaptureQueueFull {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the capture queue is full; retry after queued events are stored")
    }
}

impl std::error::Error for CaptureQueueFull {}

/// Another process held `capacity.lock` for this publisher's whole wait
/// window. Distinct from [`CaptureQueueFull`]: the queue may have room.
#[derive(Debug)]
pub(crate) struct CaptureQueueBusy;

impl std::fmt::Display for CaptureQueueBusy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the capture queue is busy; retry after the current writers finish")
    }
}

impl std::error::Error for CaptureQueueBusy {}

/// Longest wait for `capacity.lock`; a hook deadline shortens it.
const CAPACITY_LOCK_WAIT: Duration = Duration::from_secs(30);

/// Context marking a failure to write an already validated event into
/// `ready/`. Hooks that tolerate rejected events (Claude `observe`) still
/// fail on this, because exit 0 promises the event is queued.
#[derive(Debug)]
pub(crate) struct CaptureEnqueueFailed;

impl std::fmt::Display for CaptureEnqueueFailed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the capture event could not be queued")
    }
}

struct SpoolPaths {
    root: PathBuf,
    tmp: PathBuf,
    ready: PathBuf,
    quarantine: PathBuf,
}

/// Where a published envelope landed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PublishOutcome {
    /// A complete file is in `ready/`.
    Published,
    /// `ready/` already had this dedup key.
    AlreadyQueued,
}

/// Publish one complete envelope.
///
/// `terminal` selects the `0-` filename prefix so a later consumer stores it
/// before `1-` files. The same dedup key already present in `ready/` is
/// success. A crash before `rename` leaves no `ready/` file.
/// `deadline` is the hook's capture deadline, if any: waits for
/// `capacity.lock` end there (and never exceed 30 seconds), so a contended
/// queue cannot hold a hook past its host timeout. One attempt is always made.
pub(crate) fn publish(
    storage: &Path,
    terminal: bool,
    dedup_key: &str,
    body: &[u8],
    deadline: Option<std::time::Instant>,
) -> Result<PublishOutcome> {
    let started = std::time::Instant::now();
    let lock_until = deadline.map_or(started + CAPACITY_LOCK_WAIT, |deadline| {
        deadline.min(started + CAPACITY_LOCK_WAIT)
    });
    let paths = ensure_spool(storage)?;
    if ready_has_key(&paths.ready, dedup_key)? {
        return already_queued(&paths);
    }
    let name = file_name(terminal, dedup_key);
    let tmp_path = paths.tmp.join(&name);
    {
        let _lock = lock_capacity(&paths.root, lock_until)?;
        delete_expired_tmp(&paths.tmp)?;
        let tmp_count = count_files(&paths.tmp)?;
        if tmp_count >= TMP_FILE_LIMIT {
            return Err(CaptureQueueFull.into());
        }
        create_exclusive(&tmp_path)?;
    }
    pause_after_tmp_create_for_test(&paths.root);
    // From here the temp file is this publisher's: every outcome other than
    // publication removes it, so failed publishers cannot fill `tmp/`.
    let outcome = publish_tmp(&paths, &tmp_path, &name, dedup_key, body, lock_until);
    if !matches!(outcome, Ok(PublishOutcome::Published)) {
        let _ = fs::remove_file(&tmp_path);
    }
    outcome
}

/// Write, then rename one created temp file into `ready/` under the lock.
fn publish_tmp(
    paths: &SpoolPaths,
    tmp_path: &Path,
    name: &str,
    dedup_key: &str,
    body: &[u8],
    lock_until: std::time::Instant,
) -> Result<PublishOutcome> {
    write_fsync(tmp_path, body)?;
    if fail_before_rename_for_test(&paths.root) {
        return Err(anyhow!(
            "capture queue stopped before the ready file was published"
        ));
    }
    let _lock = lock_capacity(&paths.root, lock_until)?;
    if ready_has_key(&paths.ready, dedup_key)? {
        return already_queued(paths);
    }
    let (files, bytes) = combined_usage(&paths.ready, &paths.quarantine)?;
    if files >= READY_FILE_LIMIT || bytes.saturating_add(body.len() as u64) > BYTE_LIMIT {
        return Err(CaptureQueueFull.into());
    }
    let ready_path = paths.ready.join(name);
    fs::rename(tmp_path, &ready_path)
        .map_err(|_| anyhow!("capture queue could not publish the ready file"))?;
    fsync_dir(&paths.ready)?;
    Ok(PublishOutcome::Published)
}

/// A file for this dedup key is already in `ready/`. An earlier publisher
/// may have renamed it and then failed to flush the directory, so flush it
/// before reporting the event as queued.
fn already_queued(paths: &SpoolPaths) -> Result<PublishOutcome> {
    fsync_dir(&paths.ready)?;
    Ok(PublishOutcome::AlreadyQueued)
}

/// `ready/` filenames in the order a consumer must apply them.
pub(crate) fn ready_names(storage: &Path) -> Result<Vec<String>> {
    let ready = storage.join("private").join(SPOOL_DIR).join(READY_DIR);
    let mut names = list_file_names(&ready)?;
    names.sort();
    Ok(names)
}

pub(crate) fn ready_dir(storage: &Path) -> PathBuf {
    storage.join("private").join(SPOOL_DIR).join(READY_DIR)
}

pub(crate) fn quarantine_dir(storage: &Path) -> PathBuf {
    storage.join("private").join(SPOOL_DIR).join(QUARANTINE_DIR)
}

/// Variables the consumer may inherit from the hook. Everything else,
/// including provider API keys in the agent's environment, is dropped. The
/// list keeps the inputs that select configuration, identity, temporary
/// storage, durability and logging, so replay resolves the same values the
/// hook process would have used.
const CONSUMER_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "XDG_CONFIG_HOME",
    "TMPDIR",
    "TEMP",
    "TMP",
    "LIBRA_HOME",
    "LIBRA_CONFIG_GLOBAL_DB",
    "LIBRA_CONFIG_SYSTEM_DB",
    "LIBRA_COMMITTER_NAME",
    "LIBRA_COMMITTER_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "EMAIL",
    "LIBRA_SYNC_DATA",
    "LIBRA_LOG",
    "LIBRA_LOG_FILE",
    "LIBRA_LOG_ROTATION",
    "RUST_LOG",
    // Provider locations the replayed capture reads (transcripts, exports).
    "CODEX_HOME",
    "GROK_HOME",
    "HERMES_HOME",
    "OPENCODE_CONFIG_DIR",
    "OPENCODE_LIBRA_NATIVE_KEY_DIR",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    // Export sandbox selection.
    "LIBRA_SANDBOX_ENFORCEMENT",
    "LIBRA_SANDBOX_NETWORK_DISABLED",
    "LIBRA_LINUX_SANDBOX_EXE",
    "LIBRA_USE_LINUX_SANDBOX_BWRAP",
    "LIBRA_BWRAP_BINARY",
    "LIBRA_SECCOMP_POLICY",
    // Object storage the consumer writes traces objects to.
    "LIBRA_STORAGE_TYPE",
    "LIBRA_STORAGE_BUCKET",
    "LIBRA_STORAGE_ENDPOINT",
    "LIBRA_STORAGE_REGION",
    "LIBRA_STORAGE_ACCESS_KEY",
    "LIBRA_STORAGE_SECRET_KEY",
    "LIBRA_STORAGE_THRESHOLD",
    "LIBRA_STORAGE_CACHE_SIZE",
    "LIBRA_STORAGE_ALLOW_HTTP",
    "LIBRA_READ_POLICY",
    "LIBRA_MAX_CONNECTIONS",
    "LIBRA_TEST",
    "LIBRA_TEST_HOME",
];

/// Start one detached consumer: payload-free argv, the hook's verified
/// worktree as cwd, an allowlisted environment, null stdio and its own
/// session. Failure to spawn does not remove `ready/`.
pub(crate) fn try_spawn_consumer(repo_root: &Path) -> io::Result<()> {
    // A unit-test process is the test harness, not `libra`; unit tests call
    // `drain::drain_storage` directly instead.
    if cfg!(test) || consumer_spawn_disabled_for_test() {
        let _ = repo_root;
        return Ok(());
    }
    let executable = std::env::current_exe()?;
    let mut command = std::process::Command::new(executable);
    command
        .arg(DRAIN_ARG)
        .current_dir(repo_root)
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for name in CONSUMER_ENV_ALLOWLIST {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    forward_test_seams(&mut command);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: setsid is async-signal-safe and the child does not touch
        // parent-owned state before exec.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    // Dropping the handle neither waits for nor signals the detached child.
    let _child = command.spawn()?;
    Ok(())
}

/// Debug builds under `LIBRA_TEST=1` also forward the `LIBRA_TEST_*` fault
/// and fixture seams, so a test reaches the process that writes the catalog.
fn forward_test_seams(command: &mut std::process::Command) {
    #[cfg(debug_assertions)]
    {
        if std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_some_and(|value| value == "1") {
            for (name, value) in std::env::vars_os() {
                if name
                    .to_str()
                    .is_some_and(|name| name.starts_with("LIBRA_TEST_"))
                {
                    command.env(name, value);
                }
            }
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = command;
}

/// Debug-build seam: with `LIBRA_TEST=1` and
/// `LIBRA_TEST_CAPTURE_SPOOL_NO_CONSUMER=1` a hook queues without starting a
/// consumer, so an integration test can inspect `ready/` or run
/// `__capture-spool-drain` itself without racing a detached process.
fn consumer_spawn_disabled_for_test() -> bool {
    #[cfg(debug_assertions)]
    {
        let set = |name: &str| std::env::var_os(name).is_some_and(|value| value == "1");
        set(crate::utils::pager::LIBRA_TEST_ENV) && set("LIBRA_TEST_CAPTURE_SPOOL_NO_CONSUMER")
    }
    #[cfg(not(debug_assertions))]
    {
        false
    }
}

fn ensure_spool(storage: &Path) -> Result<SpoolPaths> {
    let private = storage.join("private");
    create_private_dir(&private)?;
    let root = private.join(SPOOL_DIR);
    create_private_dir(&root)?;
    let tmp = root.join(TMP_DIR);
    let ready = root.join(READY_DIR);
    let quarantine = root.join(QUARANTINE_DIR);
    create_private_dir(&tmp)?;
    create_private_dir(&ready)?;
    create_private_dir(&quarantine)?;
    Ok(SpoolPaths {
        root,
        tmp,
        ready,
        quarantine,
    })
}

fn create_private_dir(path: &Path) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(anyhow!(
                "capture queue directory is not a private directory"
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
            if metadata.mode() & 0o077 != 0 {
                // An existing directory open to group or others is narrowed
                // when this user owns it; another owner's directory fails
                // closed instead of receiving redacted events.
                // SAFETY: `geteuid` has no preconditions and cannot fail.
                if metadata.uid() != unsafe { libc::geteuid() } {
                    return Err(anyhow!(
                        "capture queue directory is not a private directory"
                    ));
                }
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                    .context("make the capture queue directory private")?;
            }
        }
        return Ok(());
    }
    #[cfg(unix)]
    let created = {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(path)
    };
    #[cfg(not(unix))]
    let created = fs::create_dir(path);
    match created {
        Ok(()) => Ok(()),
        // A concurrent hook created it first; it must still be a real
        // directory, not a link planted in between.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
                _ => Err(anyhow!(
                    "capture queue directory is not a private directory"
                )),
            }
        }
        Err(_) => Err(anyhow!("capture queue directory could not be created")),
    }
}

fn lock_capacity(root: &Path, until: std::time::Instant) -> Result<File> {
    let path = root.join(CAPACITY_LOCK);
    if let Ok(metadata) = fs::symlink_metadata(&path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(anyhow!("capture queue capacity lock is unsafe"));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options
        .open(&path)
        .map_err(|_| anyhow!("capture queue capacity lock could not be opened"))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => lock_capacity_wait(file, until),
        Err(std::fs::TryLockError::Error(_)) => {
            Err(anyhow!("capture queue capacity lock could not be taken"))
        }
    }
}

fn lock_capacity_wait(file: File, until: std::time::Instant) -> Result<File> {
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(CaptureQueueBusy.into());
            }
            Err(std::fs::TryLockError::Error(_)) => {
                return Err(anyhow!("capture queue capacity lock could not be taken"));
            }
        }
    }
}

fn create_exclusive(path: &Path) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(path)
        .map(|_| ())
        .map_err(|_| anyhow!("capture queue could not create a temporary file"))
}

fn write_fsync(path: &Path, body: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|_| anyhow!("capture queue could not open a temporary file"))?;
    file.write_all(body)
        .map_err(|_| anyhow!("capture queue could not write an envelope"))?;
    file.sync_all()
        .map_err(|_| anyhow!("capture queue could not store an envelope"))?;
    Ok(())
}

fn fsync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let dir = File::open(path)
            .map_err(|_| anyhow!("capture queue could not open the ready directory"))?;
        dir.sync_all()
            .map_err(|_| anyhow!("capture queue could not store the ready directory"))?;
    }
    // Windows has no portable directory flush through std; the envelope file
    // itself was flushed before the rename. The documented durability promise
    // (`docs/commands/agent.md`) is Unix-only for the directory entry.
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn delete_expired_tmp(tmp: &Path) -> Result<()> {
    let now = SystemTime::now();
    for name in list_file_names(tmp)? {
        let path = tmp.join(&name);
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if now.duration_since(modified).unwrap_or_default() > TMP_MAX_AGE {
            let _ = fs::remove_file(path);
        }
    }
    Ok(())
}

fn ready_has_key(ready: &Path, dedup_key: &str) -> Result<bool> {
    let marker = sanitize_key(dedup_key);
    Ok(list_file_names(ready)?
        .into_iter()
        .any(|name| name_has_marker(&name, &marker)))
}

fn name_has_marker(name: &str, marker: &str) -> bool {
    name.rsplit_once('~')
        .is_some_and(|(_, suffix)| suffix == marker)
}

fn combined_usage(ready: &Path, quarantine: &Path) -> Result<(usize, u64)> {
    let mut files = 0_usize;
    let mut bytes = 0_u64;
    for dir in [ready, quarantine] {
        for name in list_file_names(dir)? {
            files = files.saturating_add(1);
            if let Ok(metadata) = fs::metadata(dir.join(name)) {
                bytes = bytes.saturating_add(metadata.len());
            }
        }
    }
    Ok((files, bytes))
}

fn count_files(dir: &Path) -> Result<usize> {
    Ok(list_file_names(dir)?.len())
}

fn list_file_names(dir: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(names),
        Err(_) => return Err(anyhow!("capture queue directory could not be read")),
    };
    for entry in entries {
        let entry = entry.map_err(|_| anyhow!("capture queue directory could not be read"))?;
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() || !file_type.is_file() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            names.push(name.to_string());
        }
    }
    Ok(names)
}

fn file_name(terminal: bool, dedup_key: &str) -> String {
    let prefix = if terminal { "0-" } else { "1-" };
    format!("{prefix}{}~{}", sortable_id(), sanitize_key(dedup_key))
}

fn sortable_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let random = Uuid::new_v4().simple().to_string();
    format!("{millis:013}-{random}")
}

fn sanitize_key(dedup_key: &str) -> String {
    dedup_key
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// Test-only faults, scoped to one spool root so parallel unit tests in the
/// same process never observe each other's injected failures.
#[cfg(test)]
mod fault {
    use std::{
        path::{Path, PathBuf},
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };

    pub(super) static FAIL_BEFORE_RENAME: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    pub(super) static PAUSE_AFTER_TMP: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    pub(super) static PAUSE_ARRIVED: AtomicUsize = AtomicUsize::new(0);
    pub(super) static PAUSE_RELEASE: AtomicBool = AtomicBool::new(false);

    fn armed(list: &Mutex<Vec<PathBuf>>, root: &Path) -> bool {
        list.lock()
            .map(|roots| roots.iter().any(|armed| armed == root))
            .unwrap_or(false)
    }

    pub(super) fn pause_after_tmp(root: &Path) {
        if !armed(&PAUSE_AFTER_TMP, root) {
            return;
        }
        PAUSE_ARRIVED.fetch_add(1, Ordering::SeqCst);
        while !PAUSE_RELEASE.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub(super) fn fail_before_rename(root: &Path) -> bool {
        armed(&FAIL_BEFORE_RENAME, root)
    }
}

fn pause_after_tmp_create_for_test(root: &Path) {
    #[cfg(test)]
    fault::pause_after_tmp(root);
    #[cfg(not(test))]
    let _ = root;
}

fn fail_before_rename_for_test(root: &Path) -> bool {
    #[cfg(test)]
    {
        fault::fail_before_rename(root)
    }
    #[cfg(not(test))]
    {
        let _ = root;
        false
    }
}

/// Capture queue tests run locally only. They are skipped unless
/// `LIBRA_RUN_CAPTURE_TESTS=1`, which GitHub Actions never sets.
#[cfg(test)]
pub(crate) fn capture_tests_enabled(name: &str) -> bool {
    if std::env::var_os("LIBRA_RUN_CAPTURE_TESTS").is_some_and(|value| value == "1") {
        return true;
    }
    eprintln!("skipped ({name}: set LIBRA_RUN_CAPTURE_TESTS=1 to run local capture tests)");
    false
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier, atomic::Ordering};

    use super::*;

    fn storage() -> tempfile::TempDir {
        tempfile::tempdir().expect("spool fixture")
    }

    fn spool_root(storage: &Path) -> PathBuf {
        storage.join("private").join(SPOOL_DIR)
    }

    #[test]
    fn capture_spool_queue_full_text_omits_newer_libra() {
        if !capture_tests_enabled("capture_spool_queue_full_text_omits_newer_libra") {
            return;
        }
        let text = CaptureQueueFull.to_string();
        assert!(!text.contains("newer Libra"), "{text}");
        assert!(text.contains("queue is full"));
    }

    #[test]
    fn capture_spool_crash_before_rename_leaves_ready_empty() {
        if !capture_tests_enabled("capture_spool_crash_before_rename_leaves_ready_empty") {
            return;
        }
        let root = storage();
        fault::FAIL_BEFORE_RENAME
            .lock()
            .expect("fault list")
            .push(spool_root(root.path()));
        let error =
            publish(root.path(), false, "dedup-crash", b"CANARY-BODY", None).expect_err("stop");
        fault::FAIL_BEFORE_RENAME
            .lock()
            .expect("fault list")
            .retain(|armed| armed != &spool_root(root.path()));
        assert!(!format!("{error:#}").contains("CANARY-BODY"));
        assert!(
            ready_names(root.path()).expect("ready").is_empty(),
            "rename did not happen, so ready/ must stay empty"
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_spool_directories_and_files_are_private() {
        use std::os::unix::fs::PermissionsExt as _;

        if !capture_tests_enabled("capture_spool_directories_and_files_are_private") {
            return;
        }
        let root = storage();
        publish(root.path(), false, "private-key", b"body", None).expect("publish");
        let spool = spool_root(root.path());
        for dir in [
            &spool,
            &spool.join(TMP_DIR),
            &spool.join(READY_DIR),
            &spool.join(QUARANTINE_DIR),
        ] {
            let mode = fs::metadata(dir).expect("dir").permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", dir.display());
        }
        let name = ready_names(root.path()).expect("ready").remove(0);
        let mode = fs::metadata(ready_dir(root.path()).join(name))
            .expect("file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn capture_spool_existing_open_directories_are_made_private() {
        use std::os::unix::fs::PermissionsExt as _;

        if !capture_tests_enabled("capture_spool_existing_open_directories_are_made_private") {
            return;
        }
        let root = storage();
        let spool = spool_root(root.path());
        let ready = spool.join(READY_DIR);
        fs::create_dir_all(&ready).expect("pre-existing ready dir");
        for dir in [&spool, &ready] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("open mode");
        }
        publish(root.path(), false, "open-dir-key", b"body", None).expect("publish");
        for dir in [&spool, &ready] {
            let mode = fs::metadata(dir).expect("dir").permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", dir.display());
        }
    }

    #[test]
    fn capture_spool_lock_wait_ends_at_the_hook_deadline() {
        if !capture_tests_enabled("capture_spool_lock_wait_ends_at_the_hook_deadline") {
            return;
        }
        let root = storage();
        publish(root.path(), false, "warm-lock", b"body", None).expect("create the spool");
        let holder = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(spool_root(root.path()).join(CAPACITY_LOCK))
            .expect("open capacity lock");
        holder.lock().expect("hold capacity lock");
        let started = std::time::Instant::now();
        let error = publish(
            root.path(),
            false,
            "contended",
            b"body",
            Some(started + Duration::from_millis(50)),
        )
        .expect_err("a held lock past the deadline must fail");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the wait must end at the hook deadline"
        );
        assert!(
            error.downcast_ref::<CaptureQueueBusy>().is_some(),
            "{error:#}"
        );
        assert!(error.downcast_ref::<CaptureQueueFull>().is_none());
        drop(holder);
        let names = ready_names(root.path()).expect("ready");
        assert_eq!(
            names.len(),
            1,
            "only the warm-up entry is queued: {names:?}"
        );
    }

    #[test]
    fn capture_spool_second_lock_timeout_removes_its_tmp_file() {
        use std::sync::atomic::Ordering;

        if !capture_tests_enabled("capture_spool_second_lock_timeout_removes_its_tmp_file") {
            return;
        }
        let root = storage();
        publish(root.path(), false, "warm-second", b"body", None).expect("create the spool");
        let spool = spool_root(root.path());
        let arrived = fault::PAUSE_ARRIVED.load(Ordering::SeqCst);
        fault::PAUSE_RELEASE.store(false, Ordering::SeqCst);
        fault::PAUSE_AFTER_TMP.lock().unwrap().push(spool.clone());
        let storage_path = root.path().to_path_buf();
        let publisher = std::thread::spawn(move || {
            publish(
                &storage_path,
                false,
                "second-lock",
                b"body",
                Some(std::time::Instant::now() + Duration::from_millis(400)),
            )
        });
        while fault::PAUSE_ARRIVED.load(Ordering::SeqCst) == arrived {
            std::thread::sleep(Duration::from_millis(1));
        }
        // The publisher created its temp file; hold the lock it needs next.
        let holder = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(spool.join(CAPACITY_LOCK))
            .expect("open capacity lock");
        holder.lock().expect("hold capacity lock");
        fault::PAUSE_RELEASE.store(true, Ordering::SeqCst);
        let error = publisher
            .join()
            .expect("publisher thread")
            .expect_err("the second lock stays busy past the deadline");
        drop(holder);
        fault::PAUSE_AFTER_TMP
            .lock()
            .unwrap()
            .retain(|armed| armed != &spool);
        assert!(
            error.downcast_ref::<CaptureQueueBusy>().is_some(),
            "{error:#}"
        );
        let leftover = fs::read_dir(spool.join(TMP_DIR)).expect("tmp dir").count();
        assert_eq!(leftover, 0, "a failed publisher must remove its temp file");
        assert_eq!(ready_names(root.path()).expect("ready").len(), 1);
    }

    #[test]
    fn capture_spool_duplicate_dedup_key_is_success() {
        if !capture_tests_enabled("capture_spool_duplicate_dedup_key_is_success") {
            return;
        }
        let root = storage();
        let first =
            publish(root.path(), false, "capture-dedup-v2:abc", b"one", None).expect("first");
        let second =
            publish(root.path(), true, "capture-dedup-v2:abc", b"two", None).expect("second");
        assert_eq!(first, PublishOutcome::Published);
        assert_eq!(second, PublishOutcome::AlreadyQueued);
        let names = ready_names(root.path()).expect("names");
        assert_eq!(names.len(), 1, "{names:?}");
    }

    #[test]
    fn capture_spool_terminal_prefix_sorts_before_other_events() {
        if !capture_tests_enabled("capture_spool_terminal_prefix_sorts_before_other_events") {
            return;
        }
        let root = storage();
        publish(root.path(), false, "event-b", b"b", None).expect("ordinary");
        publish(root.path(), true, "event-a", b"a", None).expect("terminal");
        let names = ready_names(root.path()).expect("names");
        assert!(names[0].starts_with("0-"), "{names:?}");
        assert!(names[1].starts_with("1-"), "{names:?}");
    }

    #[test]
    fn capture_spool_expired_tmp_is_removed_under_capacity_lock() {
        if !capture_tests_enabled("capture_spool_expired_tmp_is_removed_under_capacity_lock") {
            return;
        }
        let root = storage();
        let paths = ensure_spool(root.path()).expect("dirs");
        let stale = paths.tmp.join("stale-tmp");
        let fresh = paths.tmp.join("fresh-tmp");
        fs::write(&stale, b"old").expect("seed tmp");
        fs::write(&fresh, b"new").expect("seed tmp");
        let old = SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
        fs::File::options()
            .write(true)
            .open(&stale)
            .expect("open stale")
            .set_modified(old)
            .expect("age stale tmp");
        publish(root.path(), false, "fresh-key", b"new", None).expect("publish");
        assert!(!stale.exists(), "tmp older than 24 hours must be deleted");
        assert!(fresh.exists(), "a recent tmp file must stay");
    }

    #[test]
    fn capture_spool_full_tmp_rejects_without_creating() {
        if !capture_tests_enabled("capture_spool_full_tmp_rejects_without_creating") {
            return;
        }
        let root = storage();
        let paths = ensure_spool(root.path()).expect("dirs");
        for index in 0..TMP_FILE_LIMIT {
            fs::write(paths.tmp.join(format!("held-{index:02}")), b"t").expect("seed tmp");
        }
        let error = publish(root.path(), false, "no-room", b"body", None).expect_err("tmp cap");
        assert!(error.downcast_ref::<CaptureQueueFull>().is_some());
        assert_eq!(
            list_file_names(&paths.tmp).expect("tmp").len(),
            TMP_FILE_LIMIT
        );
        assert!(ready_names(root.path()).expect("ready").is_empty());
    }

    #[test]
    fn capture_spool_enqueue_one_kib_within_budget_without_catalog() {
        if !capture_tests_enabled("capture_spool_enqueue_one_kib_within_budget_without_catalog") {
            return;
        }
        let root = storage();
        let body = vec![b'x'; 1024];
        publish(root.path(), false, "warm", &body, None).expect("warm");
        let started = std::time::Instant::now();
        publish(root.path(), false, "timed-kib", &body, None).expect("timed");
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(50),
            "1 KiB publish took {elapsed:?}"
        );
        assert!(!root.path().join("private").join("capture.db").exists());
        assert!(!root.path().join("libra.db").exists());
    }

    #[test]
    fn capture_spool_ready_and_quarantine_share_the_file_cap() {
        if !capture_tests_enabled("capture_spool_ready_and_quarantine_share_the_file_cap") {
            return;
        }
        let root = storage();
        let paths = ensure_spool(root.path()).expect("dirs");
        for index in 0..READY_FILE_LIMIT {
            fs::write(paths.quarantine.join(format!("q-{index:04}")), b"q").expect("seed");
        }
        let error = publish(root.path(), false, "over-cap", b"nope", None).expect_err("capped");
        assert!(error.downcast_ref::<CaptureQueueFull>().is_some());
        assert!(!format!("{error:#}").contains("newer Libra"));
        assert!(ready_names(root.path()).expect("ready").is_empty());
        assert!(list_file_names(&paths.tmp).expect("tmp").is_empty());
    }

    #[test]
    fn capture_spool_byte_cap_counts_ready_and_quarantine() {
        if !capture_tests_enabled("capture_spool_byte_cap_counts_ready_and_quarantine") {
            return;
        }
        let root = storage();
        let paths = ensure_spool(root.path()).expect("dirs");
        let huge = paths.quarantine.join("0-huge-seed");
        fs::write(&huge, b"x").expect("seed");
        fs::File::options()
            .write(true)
            .open(&huge)
            .expect("open")
            .set_len(BYTE_LIMIT)
            .expect("sparse length");
        let error = publish(root.path(), false, "over-bytes", b"y", None).expect_err("bytes");
        assert!(error.downcast_ref::<CaptureQueueFull>().is_some());
        assert!(ready_names(root.path()).expect("ready").is_empty());
    }

    #[test]
    fn capture_spool_concurrent_tmp_create_stops_at_64() {
        if !capture_tests_enabled("capture_spool_concurrent_tmp_create_stops_at_64") {
            return;
        }
        let root = storage();
        let root_path = root.path().to_path_buf();
        let spool = spool_root(&root_path);
        fs::create_dir_all(&spool).expect("spool");
        fault::PAUSE_ARRIVED.store(0, Ordering::SeqCst);
        fault::PAUSE_RELEASE.store(false, Ordering::SeqCst);
        fault::PAUSE_AFTER_TMP
            .lock()
            .expect("fault list")
            .push(spool.clone());
        let creators = TMP_FILE_LIMIT + 16;
        let barrier = Arc::new(Barrier::new(creators));
        let mut handles = Vec::new();
        for index in 0..creators {
            let storage = root_path.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                publish(&storage, false, &format!("tmp-{index}"), b"body", None)
            }));
        }
        let started = std::time::Instant::now();
        while fault::PAUSE_ARRIVED.load(Ordering::SeqCst) < TMP_FILE_LIMIT
            && started.elapsed() < Duration::from_secs(20)
        {
            std::thread::sleep(Duration::from_millis(2));
        }
        // Every creator has either created its tmp file and paused, or been
        // refused under the lock. Give refusals a moment to return.
        std::thread::sleep(Duration::from_millis(200));
        let observed = list_file_names(&spool.join(TMP_DIR)).expect("tmp").len();
        assert_eq!(
            observed, TMP_FILE_LIMIT,
            "tmp/ must hold exactly the {TMP_FILE_LIMIT} admitted creators"
        );
        fault::PAUSE_RELEASE.store(true, Ordering::SeqCst);
        let mut full = 0;
        for handle in handles {
            if let Err(error) = handle.join().expect("join") {
                assert!(
                    error.downcast_ref::<CaptureQueueFull>().is_some(),
                    "{error:?}"
                );
                full += 1;
            }
        }
        fault::PAUSE_AFTER_TMP
            .lock()
            .expect("fault list")
            .retain(|armed| armed != &spool);
        assert_eq!(full, creators - TMP_FILE_LIMIT);
        assert_eq!(
            ready_names(&root_path).expect("ready").len(),
            TMP_FILE_LIMIT
        );
        assert!(
            list_file_names(&spool.join(TMP_DIR))
                .expect("tmp")
                .is_empty()
        );
    }

    #[test]
    fn capture_spool_concurrent_publish_stops_at_the_ready_cap() {
        if !capture_tests_enabled("capture_spool_concurrent_publish_stops_at_the_ready_cap") {
            return;
        }
        let root = storage();
        let paths = ensure_spool(root.path()).expect("dirs");
        for index in 0..(READY_FILE_LIMIT - 2) {
            fs::write(
                paths.ready.join(format!("1-seed-{index:04}~seed{index}")),
                b"s",
            )
            .expect("seed");
        }
        let racers = 8;
        let barrier = Arc::new(Barrier::new(racers));
        let mut handles = Vec::new();
        for index in 0..racers {
            let storage = root.path().to_path_buf();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                publish(&storage, false, &format!("race-{index}"), b"z", None)
            }));
        }
        let mut published = 0;
        for handle in handles {
            match handle.join().expect("join") {
                Ok(_) => published += 1,
                Err(error) => {
                    assert!(
                        error.downcast_ref::<CaptureQueueFull>().is_some(),
                        "{error:?}"
                    )
                }
            }
        }
        assert_eq!(published, 2, "only the two remaining slots may publish");
        assert_eq!(
            ready_names(root.path()).expect("names").len(),
            READY_FILE_LIMIT
        );
        assert!(list_file_names(&paths.tmp).expect("tmp").is_empty());
    }
}
