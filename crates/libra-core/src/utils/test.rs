//! Test support utilities including change-dir guards, repository setup/cleanup helpers, fixture copying, and isolated command execution helpers.

/// Return the repository workspace root directory compiled in via `build.rs`.
pub fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("LIBRA_WORKSPACE_ROOT"))
}

/// Resolve the real `libra` binary for unit tests that re-exec it.
///
/// Cargo 1.100 places a package's test harness under
/// `target/<profile>/build/<pkg>/<hash>/out`, so walking up a fixed two
/// levels no longer reaches `target/<profile>`. Walk the ancestors and take
/// the first directory that actually contains the built binary.
pub fn real_libra_helper_binary() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("locate the unit test binary");
    let name = format!("libra{}", std::env::consts::EXE_SUFFIX);
    for ancestor in exe.ancestors().skip(1) {
        let candidate = ancestor.join(&name);
        if candidate.is_file() {
            return candidate;
        }
    }
    let candidate = workspace_root().join("target/debug").join(&name);
    if candidate.is_file() {
        return candidate;
    }
    panic!(
        "the real Libra helper binary must be built before this focused unit test: exe={}",
        exe.display()
    );
}

use std::{
    cell::Cell,
    env,
    ffi::{OsStr, OsString},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Condvar, Mutex, Once, OnceLock},
};

use tracing::level_filters::LevelFilter;

use crate::{
    internal::config::ConfigKv,
    utils::{pager::LIBRA_TEST_ENV, util},
};

static MARK_TEST_NON_INTERACTIVE: Once = Once::new();

static CWD_LOCK: OnceLock<CwdLock> = OnceLock::new();
thread_local! {
    static CWD_LOCK_OWNER: Cell<Option<u64>> = const { Cell::new(None) };
}

struct CwdLock {
    state: Mutex<CwdLockState>,
    available: Condvar,
}

struct CwdLockState {
    owner: Option<u64>,
    depth: usize,
    next_owner: u64,
}

pub struct CwdLockGuard {
    lock: &'static CwdLock,
    owner: u64,
}

impl CwdLock {
    fn acquire(&'static self) -> CwdLockGuard {
        let current_owner = CWD_LOCK_OWNER.with(Cell::get);
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());

        loop {
            match state.owner {
                None => {
                    let owner = state.next_owner;
                    state.next_owner = state.next_owner.wrapping_add(1).max(1);
                    state.owner = Some(owner);
                    state.depth = 1;
                    CWD_LOCK_OWNER.with(|current| current.set(Some(owner)));
                    return CwdLockGuard { lock: self, owner };
                }
                Some(owner) if Some(owner) == current_owner => {
                    state.depth += 1;
                    return CwdLockGuard { lock: self, owner };
                }
                Some(_) => {
                    state = self
                        .available
                        .wait(state)
                        .unwrap_or_else(|err| err.into_inner());
                }
            }
        }
    }

    #[cfg(any(test, feature = "test-helpers"))]
    fn try_acquire(&'static self) -> Option<CwdLockGuard> {
        let current_owner = CWD_LOCK_OWNER.with(Cell::get);
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());

        match state.owner {
            None => {
                let owner = state.next_owner;
                state.next_owner = state.next_owner.wrapping_add(1).max(1);
                state.owner = Some(owner);
                state.depth = 1;
                CWD_LOCK_OWNER.with(|current| current.set(Some(owner)));
                Some(CwdLockGuard { lock: self, owner })
            }
            Some(owner) if Some(owner) == current_owner => {
                state.depth += 1;
                Some(CwdLockGuard { lock: self, owner })
            }
            Some(_) => None,
        }
    }

    fn release(&self, owner: u64) {
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());

        if state.owner != Some(owner) {
            return;
        }

        state.depth -= 1;
        if state.depth == 0 {
            state.owner = None;
            self.available.notify_one();
            CWD_LOCK_OWNER.with(|current| {
                if current.get() == Some(owner) {
                    current.set(None);
                }
            });
        }
    }
}

impl Drop for CwdLockGuard {
    fn drop(&mut self) {
        self.lock.release(self.owner);
    }
}

fn cwd_lock() -> CwdLockGuard {
    CWD_LOCK
        .get_or_init(|| CwdLock {
            state: Mutex::new(CwdLockState {
                owner: None,
                depth: 0,
                next_owner: 1,
            }),
            available: Condvar::new(),
        })
        .acquire()
}

#[cfg(any(test, feature = "test-helpers"))]
fn try_cwd_lock() -> Option<CwdLockGuard> {
    CWD_LOCK
        .get_or_init(|| CwdLock {
            state: Mutex::new(CwdLockState {
                owner: None,
                depth: 0,
                next_owner: 1,
            }),
            available: Condvar::new(),
        })
        .try_acquire()
}

pub fn cwd_lock_guard() -> CwdLockGuard {
    cwd_lock()
}

#[cfg(any(test, feature = "test-helpers"))]
#[doc(hidden)]
pub fn try_cwd_lock_guard() -> Option<CwdLockGuard> {
    try_cwd_lock()
}

pub struct ScopedEnvVar {
    key: String,
    previous: Option<OsString>,
}

impl ScopedEnvVar {
    pub fn set(key: impl Into<String>, value: impl AsRef<OsStr>) -> Self {
        let key = key.into();
        let previous = env::var_os(&key);
        // SAFETY: command tests mutate process env only in controlled test flows.
        unsafe {
            env::set_var(&key, value.as_ref());
        }
        Self { key, previous }
    }

    /// Remove `key` for the guard's lifetime, restoring the previous value on drop.
    pub fn unset(key: impl Into<String>) -> Self {
        let key = key.into();
        let previous = env::var_os(&key);
        // SAFETY: command tests mutate process env only in controlled test flows.
        unsafe {
            env::remove_var(&key);
        }
        Self { key, previous }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        // SAFETY: this restores the exact previous value for the same process env key.
        unsafe {
            if let Some(value) = &self.previous {
                env::set_var(&self.key, value);
            } else {
                env::remove_var(&self.key);
            }
        }
    }
}

/// Process-environment sandbox for tests that can open global or system config.
///
/// The fixture owns its temporary root instead of accepting a caller-provided
/// path, so ambient user and system configuration paths cannot be selected by
/// mistake. Callers must use the `serial(env)` lane because environment
/// variables are process-global.
pub struct ConfigDbFixture {
    // Drop the guards before TempDir removes the paths they reference.
    _env: [ScopedEnvVar; 5],
    home: PathBuf,
    xdg_config_home: PathBuf,
    global_db: PathBuf,
    system_db: PathBuf,
    root: tempfile::TempDir,
}

impl ConfigDbFixture {
    pub fn new() -> std::io::Result<Self> {
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let xdg_config_home = root.path().join("xdg");
        let global_db = root.path().join("global").join("config.db");
        let system_db = root.path().join("system").join("config.db");
        fs::create_dir_all(&home)?;
        fs::create_dir_all(&xdg_config_home)?;
        if let Some(parent) = global_db.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Some(parent) = system_db.parent() {
            fs::create_dir_all(parent)?;
        }

        let env = [
            ScopedEnvVar::set("LIBRA_CONFIG_GLOBAL_DB", &global_db),
            ScopedEnvVar::set("LIBRA_CONFIG_SYSTEM_DB", &system_db),
            ScopedEnvVar::set("HOME", &home),
            ScopedEnvVar::set("USERPROFILE", &home),
            ScopedEnvVar::set("XDG_CONFIG_HOME", &xdg_config_home),
        ];

        Ok(Self {
            _env: env,
            home,
            xdg_config_home,
            global_db,
            system_db,
            root,
        })
    }

    pub fn root(&self) -> &Path {
        self.root.path()
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn xdg_config_home(&self) -> &Path {
        &self.xdg_config_home
    }

    pub fn global_db(&self) -> &Path {
        &self.global_db
    }

    pub fn system_db(&self) -> &Path {
        &self.system_db
    }

    pub fn contains(&self, path: impl AsRef<Path>) -> bool {
        path.as_ref().starts_with(self.root())
    }
}

pub struct ChangeDirGuard {
    old_dir: PathBuf,
    _cwd_lock: CwdLockGuard,
}

impl ChangeDirGuard {
    /// Creates a new `ChangeDirGuard` that changes the current directory to `new_dir`.
    /// This will automatically change the directory back to the original one when the guard is dropped.
    ///
    /// The guard serializes process-wide current-directory changes so parallel
    /// tests using `ChangeDirGuard` do not observe each other's repositories.
    ///
    /// # Arguments
    ///
    /// * `new_dir` - The new directory to change to.
    ///
    /// # Returns
    ///
    /// * A `ChangeDirGuard` instance that will change the directory back to the original one when dropped.
    ///
    pub fn new(new_dir: impl AsRef<Path>) -> Self {
        let cwd_lock = cwd_lock();
        let old_dir = env::current_dir().unwrap_or_else(|_| find_cargo_dir());
        env::set_current_dir(new_dir).unwrap();
        Self {
            old_dir,
            _cwd_lock: cwd_lock,
        }
    }
}

impl Drop for ChangeDirGuard {
    fn drop(&mut self) {
        let fallback = find_cargo_dir_opt().unwrap_or_else(std::env::temp_dir);
        let target = if self.old_dir.exists() {
            &self.old_dir
        } else {
            // Temp test directories may already be gone when the guard drops.
            &fallback
        };
        // Silently ignore errors to avoid aborting during stack unwinding.
        let _ = env::set_current_dir(target);
    }
}

/// Returns `Some(path)` to the workspace root (containing `Cargo.toml`),
/// or `None` if it cannot be determined.
fn find_cargo_dir_opt() -> Option<PathBuf> {
    let root = workspace_root();
    if root.join("Cargo.toml").exists() {
        return Some(root);
    }
    let mut path = util::cur_dir();
    loop {
        path.push("Cargo.toml");
        if path.exists() {
            path.pop();
            return Some(path);
        }
        if !path.pop() || !path.pop() {
            return None;
        }
    }
}

pub fn find_cargo_dir() -> PathBuf {
    find_cargo_dir_opt().expect("Could not find LIBRA_WORKSPACE_ROOT")
}

/// Sets up a clean environment for testing.
///
/// This function first calls `setup_env()` to switch the current directory to the test directory.
/// Then, it checks if the Libra root directory (`.libra`) exists in the current directory.
/// If it does, the function removes the entire `.libra` directory.
pub fn setup_clean_testing_env_in(temp_path: impl AsRef<Path>) {
    mark_test_process_non_interactive();

    assert!(temp_path.as_ref().exists(), "temp_path does not exist");
    assert!(temp_path.as_ref().is_dir(), "temp_path is not a directory");
    assert!(
        temp_path.as_ref().read_dir().unwrap().count() == 0,
        "temp_path is not empty"
    );

    tracing::info!("Using libra testing path: {:?}", temp_path.as_ref());

    // Define the directories that are present in a bare repository
    let owned = temp_path.as_ref().to_path_buf();
    let bare_repo_dirs = ["objects", "info", "description", "libra.db"];

    // Remove the directories that are present in a bare repository if they exist
    for dir in bare_repo_dirs.iter() {
        let bare_repo_path = owned.join(dir);
        if bare_repo_path.exists() && bare_repo_path.is_dir() {
            fs::remove_dir_all(&bare_repo_path).unwrap();
        } else if bare_repo_path.exists() && !bare_repo_path.is_dir() {
            // Remove the file if it exists
            fs::remove_file(&bare_repo_path).unwrap();
        }
    }
}

fn mark_test_process_non_interactive() {
    MARK_TEST_NON_INTERACTIVE.call_once(|| {
        // SAFETY: this process-wide default is set exactly once for the test process.
        unsafe {
            env::set_var(LIBRA_TEST_ENV, "1");
        }
    });
}

/// Low-level repository bootstrap used by `libra-core` unit tests without
/// depending on `command::init`. Matches the default non-bare `libra init`
/// layout, SQLite schema, core config keys, and initial `HEAD -> main` ref.
async fn init_minimal_repo_for_test(repo_dir: &Path) {
    use sea_orm::{ActiveModelTrait, Set};

    let root_dir = repo_dir.join(util::ROOT_DIR);
    fs::create_dir_all(root_dir.join("objects/info")).expect("create objects/info");
    fs::create_dir_all(root_dir.join("objects/pack")).expect("create objects/pack");
    fs::create_dir_all(root_dir.join("info")).expect("create info");
    fs::create_dir_all(root_dir.join("hooks")).expect("create hooks");

    let db_path = root_dir.join(util::DATABASE);
    let conn =
        crate::internal::db::create_database(db_path.to_str().expect("utf-8 test database path"))
            .await
            .expect("create test libra database");

    let ignore_case = crate::utils::path_case::probe_dir_ignore_case(repo_dir);
    let ignorecase_text = if ignore_case { "true" } else { "false" };
    let txn = crate::internal::db::begin_write_transaction(&conn)
        .await
        .expect("begin init_minimal_repo_for_test transaction");

    #[cfg(not(target_os = "windows"))]
    let entries = [
        ("core.repositoryformatversion", "0"),
        ("core.filemode", "true"),
        ("core.bare", "false"),
        ("core.logallrefupdates", "true"),
        ("core.ignorecase", ignorecase_text),
    ];
    #[cfg(target_os = "windows")]
    let entries = [
        ("core.repositoryformatversion", "0"),
        ("core.filemode", "false"),
        ("core.bare", "false"),
        ("core.logallrefupdates", "true"),
        ("core.symlinks", "false"),
        ("core.ignorecase", ignorecase_text),
    ];

    for (key, value) in entries {
        ConfigKv::set_with_conn(&txn, key, value, false)
            .await
            .expect("seed core config");
    }
    ConfigKv::set_with_conn(&txn, "core.objectformat", "sha1", false)
        .await
        .expect("seed core.objectformat");
    ConfigKv::set_with_conn(&txn, "core.initrefformat", "strict", false)
        .await
        .expect("seed core.initrefformat");
    let repo_id = uuid::Uuid::new_v4().to_string();
    ConfigKv::set_with_conn(&txn, "libra.repoid", &repo_id, false)
        .await
        .expect("seed libra.repoid");
    ConfigKv::set_with_conn(&txn, "vault.signing", "false", false)
        .await
        .expect("seed vault.signing");

    let head = libra_model::reference::ActiveModel {
        name: Set(Some("main".to_string())),
        kind: Set(libra_model::reference::ConfigKind::Head),
        commit: Set(None),
        remote: Set(None),
        worktree_id: Set(None),
        ..Default::default()
    };
    head.insert(&txn).await.expect("seed initial HEAD ref");
    txn.commit()
        .await
        .expect("commit init_minimal_repo_for_test");
}

/// switch to test dir and create a new .libra
pub async fn setup_with_new_libra_in(temp_path: impl AsRef<Path>) {
    setup_clean_testing_env_in(temp_path.as_ref());
    util::reset_objects_storage_cache_for_path(
        &temp_path.as_ref().join(util::ROOT_DIR).join("objects"),
    );
    let _guard = ChangeDirGuard::new(temp_path.as_ref());
    init_minimal_repo_for_test(temp_path.as_ref()).await;
    util::reset_objects_storage_cache_for_path(
        &temp_path.as_ref().join(util::ROOT_DIR).join("objects"),
    );

    // Most tests don't exercise identity flows. Seed a deterministic identity so
    // commit-related tests don't depend on host-level config.
    let _ = ConfigKv::set("user.name", "Libra Test User", false).await;
    let _ = ConfigKv::set("user.email", "libra-test@example.com", false).await;
}
/// change the log level to reduce verbose output.
pub fn init_debug_logger() {
    init_logger_with_default_level(LevelFilter::DEBUG);
}

pub fn init_logger() {
    init_logger_with_default_level(LevelFilter::INFO);
}

fn init_logger_with_default_level(default_level: LevelFilter) {
    let effective_level =
        if env::var_os("LIBRA_TEST_LOG").is_some() || env::var_os("RUST_LOG").is_some() {
            default_level
        } else {
            // Keep tests quiet by default; opt in with LIBRA_TEST_LOG=1 when debugging.
            LevelFilter::OFF
        };

    let _ = tracing_subscriber::fmt()
        .with_max_level(effective_level)
        .try_init(); // avoid multi-init
}

/// create file related to working directory
pub fn ensure_file(path: impl AsRef<Path>, content: Option<&str>) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap(); // ensure父目录
    let mut file = fs::File::create(util::working_dir().join(path))
        .unwrap_or_else(|_| panic!("Cannot create file：{path:?}"));
    if let Some(content) = content {
        file.write_all(content.as_bytes()).unwrap();
    } else {
        // write filename if no content
        file.write_all(path.file_name().unwrap().as_encoded_bytes())
            .unwrap();
    }
}

/// reset working directory to the root of the module
pub fn reset_working_dir() {
    // Keep this legacy helper in the same process-wide CWD protocol as
    // ChangeDirGuard, so a future caller cannot race another test's guard.
    let _cwd_lock = cwd_lock();
    env::set_current_dir(env!("LIBRA_WORKSPACE_ROOT")).unwrap();
}
