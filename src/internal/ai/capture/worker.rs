//! Detached, content-free SessionStart recovery worker ownership.

use std::{
    fs::{self, File, OpenOptions},
    io,
    path::Path,
    process::{Command, Stdio},
};

pub(crate) const WORKER_ARG: &str = "__capture-recovery-worker";
const LOCK_NAME: &str = "agent-capture-recovery-worker.lock";
/// Debug-build-only hold point for the end-to-end ownership test; see
/// [`hold_for_test`]. Release builds never forward or read it.
#[cfg(debug_assertions)]
const TEST_HOLD_ENV: &str = "LIBRA_TEST_CAPTURE_WORKER_HOLD_PATH";

/// Best-effort parent-side exclusion avoids launching duplicate children in
/// the common case. The child independently acquires the same OS advisory
/// lock, so a crash between spawn and handoff cannot create two active workers.
pub(crate) fn spawn_detached(storage: &Path, repo_root: &Path) -> io::Result<()> {
    let Some(_lock) = try_lock(storage)? else {
        return Ok(());
    };
    let executable = std::env::current_exe()?;
    // Dropping the handle neither waits for nor signals the detached child.
    let _child = detached_command(&executable, repo_root).spawn()?;
    Ok(())
}

/// The fixed child invocation: payload-free argv, verified cwd, empty
/// environment, null stdio and its own session. Descriptors Libra opens are
/// close-on-exec (std's default; the lock file adds `O_CLOEXEC`), so neither
/// the parent's lock nor a source descriptor reaches the child.
fn detached_command(executable: &Path, repo_root: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .arg(WORKER_ARG)
        .current_dir(repo_root)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(debug_assertions)]
    forward_test_hold(&mut command);
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
    command
}

#[cfg(debug_assertions)]
fn forward_test_hold(command: &mut Command) {
    if std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        return;
    }
    if let Some(path) = std::env::var_os(TEST_HOLD_ENV) {
        command
            .env(crate::utils::pager::LIBRA_TEST_ENV, "1")
            .env(TEST_HOLD_ENV, path);
    }
}

/// Debug-build seam for the detached-ownership integration test: with
/// `LIBRA_TEST=1` and a forwarded hold path, the child keeps its worker lock
/// until that path exists (at most 30 seconds), so the test can observe the
/// child outliving the hook parent before any recovery work starts.
#[cfg(debug_assertions)]
pub(crate) async fn hold_for_test() {
    if std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        return;
    }
    let Some(path) = std::env::var_os(TEST_HOLD_ENV) else {
        return;
    };
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while fs::symlink_metadata(&path).is_err() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// Acquire the per-repository worker lock. `None` means another worker owns
/// it. The descriptor is the lock guard; OS process teardown releases it.
pub(crate) fn try_lock(storage: &Path) -> io::Result<Option<File>> {
    let private = storage.join("private");
    match fs::symlink_metadata(&private) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(io::Error::other(
                "capture recovery lock directory is unsafe",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_private_directory(&private)?;
        }
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = fs::symlink_metadata(&private)?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(io::Error::other(
                "capture recovery lock directory has unsafe ownership or permissions",
            ));
        }
    }

    let path = private.join(LOCK_NAME);
    if let Ok(metadata) = fs::symlink_metadata(&path)
        && (metadata.file_type().is_symlink() || !metadata.file_type().is_file())
    {
        return Err(io::Error::other("capture recovery lock file is unsafe"));
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
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(&path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::other("capture recovery lock file is unsafe"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
        {
            return Err(io::Error::other(
                "capture recovery lock file has unsafe ownership or permissions",
            ));
        }
    }
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error),
        }
    }
    #[cfg(not(unix))]
    {
        match fs::create_dir(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        process::{Command, Stdio},
        sync::{Arc, Barrier},
        thread,
        time::{Duration, Instant},
    };

    use super::{LOCK_NAME, WORKER_ARG, try_lock};

    #[test]
    fn advisory_lock_allows_only_one_active_owner_and_releases_on_drop() {
        let root = tempfile::tempdir().expect("create worker-lock fixture");
        let first = try_lock(root.path())
            .expect("acquire first worker lock")
            .expect("first worker lock is available");
        assert!(
            try_lock(root.path())
                .expect("probe concurrent worker lock")
                .is_none(),
            "one repository cannot have two active capture workers"
        );
        drop(first);
        assert!(
            try_lock(root.path())
                .expect("reacquire released worker lock")
                .is_some(),
            "process/guard exit releases the advisory lock"
        );
    }

    #[test]
    fn concurrent_recovery_triggers_have_one_active_worker() {
        const CONTENDERS: usize = 16;
        let root = tempfile::tempdir().expect("create concurrent worker fixture");
        let barrier = Arc::new(Barrier::new(CONTENDERS + 1));
        let mut contenders = Vec::with_capacity(CONTENDERS);
        for _ in 0..CONTENDERS {
            let storage = root.path().to_path_buf();
            let barrier = Arc::clone(&barrier);
            contenders.push(thread::spawn(move || {
                barrier.wait();
                try_lock(&storage).expect("contending worker lock attempt")
            }));
        }
        barrier.wait();

        let mut owners = Vec::new();
        for contender in contenders {
            if let Some(lock) = contender.join().expect("join worker contender") {
                owners.push(lock);
            }
        }
        assert_eq!(
            owners.len(),
            1,
            "only one trigger may own the repository worker lock"
        );
    }

    #[test]
    fn subprocess_lock_is_released_after_worker_process_is_killed() {
        const CHILD_MARKER: &str = "LIBRA_TEST_CAPTURE_WORKER_LOCK_CHILD";
        let Ok(storage) = std::env::var(CHILD_MARKER) else {
            return;
        };
        let lock = try_lock(std::path::Path::new(&storage))
            .expect("child acquires worker lock")
            .expect("child lock is available");
        let _lock = lock;
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    }

    #[test]
    fn killed_worker_process_releases_os_advisory_lock() {
        let root = tempfile::tempdir().expect("create subprocess lock fixture");
        let test_executable = std::env::current_exe().expect("resolve test executable");
        let mut child = Command::new(test_executable)
            .args([
                "--exact",
                "internal::ai::capture::worker::tests::subprocess_lock_is_released_after_worker_process_is_killed",
                "--nocapture",
            ])
            .env("LIBRA_TEST_CAPTURE_WORKER_LOCK_CHILD", root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn lock-owning subprocess");

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut observed_owned = false;
        while Instant::now() < deadline {
            match try_lock(root.path()).expect("probe subprocess lock") {
                None => {
                    observed_owned = true;
                    break;
                }
                Some(lock) => drop(lock),
            }
            if let Some(status) = child.try_wait().expect("check lock child") {
                panic!("lock child exited before acquiring its lock: {status}");
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(observed_owned, "child process must acquire the OS lock");

        child.kill().expect("terminate lock-owning subprocess");
        child.wait().expect("reap lock-owning subprocess");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(lock) = try_lock(root.path()).expect("reacquire after child exit") {
                drop(lock);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "OS releases the lock after process death"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn lock_rejects_symlinked_private_directory_or_lock_file() {
        let root = tempfile::tempdir().expect("create worker-lock fixture");
        let target = tempfile::tempdir().expect("create external lock target");
        #[cfg(unix)]
        std::os::unix::fs::symlink(target.path(), root.path().join("private"))
            .expect("create private symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(target.path(), root.path().join("private"))
            .expect("create private symlink");
        assert!(try_lock(root.path()).is_err());

        let second = tempfile::tempdir().expect("create second lock fixture");
        fs::create_dir(second.path().join("private")).expect("create private directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(
                second.path().join("private"),
                fs::Permissions::from_mode(0o700),
            )
            .expect("secure private directory");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            PathBuf::from(target.path()).join("lock"),
            second.path().join("private").join(LOCK_NAME),
        )
        .expect("create lock symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(
            PathBuf::from(target.path()).join("lock"),
            second.path().join("private").join(LOCK_NAME),
        )
        .expect("create lock symlink");
        assert!(try_lock(second.path()).is_err());
    }

    #[test]
    fn worker_entry_token_is_fixed_and_contains_no_session_payload() {
        assert_eq!(WORKER_ARG, "__capture-recovery-worker");
        assert!(!WORKER_ARG.contains(std::path::MAIN_SEPARATOR));
    }

    /// Stand-in worker: `/bin/sh` resolves the fixed token as a script in
    /// the child's cwd, records what the production invocation handed it,
    /// then stays alive until the test creates `release` (bounded).
    #[cfg(unix)]
    const STAND_IN_WORKER: &str = r#"if [ /dev/fd/0 -ef /dev/null ]; then stdin=null; else stdin=other; fi
if [ /dev/fd/1 -ef /dev/null ]; then stdout=null; else stdout=other; fi
if [ /dev/fd/2 -ef /dev/null ]; then stderr=null; else stderr=other; fi
printf 'argv0=%s\nargc=%s\ncwd=%s\nstdin=%s\nstdout=%s\nstderr=%s\nhome=%s\n' \
  "$0" "$#" "$(pwd -P)" "$stdin" "$stdout" "$stderr" "${HOME-unset}" > report.tmp
/bin/mv report.tmp report
i=0
while [ ! -e release ] && [ "$i" -lt 300 ]; do
  /bin/sleep 0.1
  i=$((i + 1))
done
"#;

    /// Reaps a stand-in child even when an assertion fails first.
    #[cfg(unix)]
    struct KillOnDrop(std::process::Child);

    #[cfg(unix)]
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            if matches!(self.0.try_wait(), Ok(None)) {
                let _ = self.0.kill();
            }
            let _ = self.0.wait();
        }
    }

    /// Runs the production child invocation (`detached_command`) against a
    /// stand-in executable while the parent holds the worker lock and an
    /// open pipe, as the SessionStart hook does at spawn time.
    #[cfg(unix)]
    #[test]
    fn detached_child_gets_null_stdio_own_session_and_no_parent_descriptors() {
        use std::{io::Read as _, os::fd::AsRawFd as _, path::Path, sync::mpsc};

        use super::detached_command;

        let root = tempfile::tempdir().expect("create detached-spawn fixture");
        let storage = root.path().join("storage");
        fs::create_dir(&storage).expect("create storage fixture");
        let repo = root.path().join("repo");
        fs::create_dir(&repo).expect("create repo fixture");
        let repo = repo.canonicalize().expect("canonicalize repo fixture");
        fs::write(repo.join(WORKER_ARG), STAND_IN_WORKER).expect("write stand-in worker");

        let parent_lock = try_lock(&storage)
            .expect("acquire parent-side worker lock")
            .expect("worker lock is free");
        // SAFETY: F_GETFD only reads the descriptor flags of an owned fd.
        let flags = unsafe { libc::fcntl(parent_lock.as_raw_fd(), libc::F_GETFD) };
        assert!(
            flags >= 0 && flags & libc::FD_CLOEXEC != 0,
            "the worker lock descriptor must be close-on-exec"
        );
        let (mut reader, writer) = std::io::pipe().expect("create parent-held pipe");

        let mut child = KillOnDrop(
            detached_command(Path::new("/bin/sh"), &repo)
                .spawn()
                .expect("spawn stand-in worker"),
        );
        drop(writer);
        drop(parent_lock);

        // A child holding the write end keeps this read blocked until it exits.
        let (eof_tx, eof_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut sink = Vec::new();
            let _ = eof_tx.send(reader.read_to_end(&mut sink).map(|_| sink.len()));
        });
        let eof = eof_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the child must not inherit the parent's open pipe descriptor")
            .expect("read parent-held pipe to EOF");
        assert_eq!(eof, 0, "nothing writes to the parent-held pipe");
        assert!(
            child.0.try_wait().expect("poll stand-in worker").is_none(),
            "EOF must be observed while the detached child is still running"
        );
        assert!(
            try_lock(&storage)
                .expect("probe worker lock after parent release")
                .is_some(),
            "the child must not inherit the parent's lock descriptor"
        );

        let pid = libc::pid_t::try_from(child.0.id()).expect("child pid fits pid_t");
        // SAFETY: getsid/getpgid only query the process table.
        let (child_sid, child_pgid, parent_sid) =
            unsafe { (libc::getsid(pid), libc::getpgid(pid), libc::getsid(0)) };
        assert_eq!(child_sid, pid, "the child must lead a new session");
        assert_eq!(child_pgid, pid, "the child must lead its own process group");
        assert_ne!(
            child_sid, parent_sid,
            "the child must leave the parent session"
        );

        let report_path = repo.join("report");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !report_path.exists() {
            assert!(
                Instant::now() < deadline,
                "stand-in worker never reported its invocation"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let report = fs::read_to_string(&report_path).expect("read stand-in report");
        let expected = format!(
            "argv0={WORKER_ARG}\nargc=0\ncwd={}\nstdin=null\nstdout=null\nstderr=null\nhome=unset\n",
            repo.display()
        );
        assert_eq!(
            report, expected,
            "fixed argv, verified cwd, null stdio and an empty environment"
        );

        fs::write(repo.join("release"), b"").expect("release stand-in worker");
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.0.try_wait().expect("poll released worker") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "released stand-in worker must exit"
            );
            thread::sleep(Duration::from_millis(10));
        };
        assert!(status.success(), "stand-in worker exited with {status}");
    }
}
