//! Initializes a repository by creating .libra storage, seeding HEAD and
//! default refs/config, and preparing the backing database.
//!
//! Error rendering and stable-code expectations are part of the CLI contract:
//! see `docs/development/cli-error-contract-design.md`.

use std::{
    env, fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

use clap::{Parser, ValueEnum};
use git_internal::hash::{HashKind, set_hash_kind};
use sea_orm::{ActiveModelTrait, DbConn, DbErr, Set, TransactionTrait};
use serde::Serialize;

use crate::{
    internal::{
        config::{ConfigKv, LocalIdentityTarget, resolve_user_identity_sources},
        db::{self, get_db_conn_instance_for_path},
        head::Head,
        model::{config, reference},
    },
    utils::{
        convert,
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, ProgressMode, emit_json_data},
        util::{DATABASE, ROOT_DIR, cur_dir},
    },
};

const DEFAULT_BRANCH: &str = "main";
const ISSUE_URL: &str = "https://github.com/libra-tools/libra/issues";
const EXAMPLES: &str = r#"EXAMPLES:
    libra init                                 Initialize in current directory
    libra init my-project                      Initialize in a new directory
    libra init --bare my-repo.git              Create a bare repository
    libra init -b develop                      Use 'develop' as initial branch
    libra init --from-git-repository ../old    Convert from existing Git repo
    libra init --vault false                   Skip vault / GPG setup
    libra init --object-format sha256          Use SHA-256 hashing"#;

// NOTE: `src/command/init.rs` lines 3-20 are a protected merge-conflict block in this workspace.
// The imports inside that block must stay as-is. To avoid `unused_imports` warnings without
// changing that block, we reference the imported symbols here in a private, dead-code helper.
#[allow(dead_code, deprecated)]
fn _touch_conflict_imports() {
    let _ = env::current_dir;
    let _ = DATABASE;
    let _ = cur_dir();
    let _ = db::create_database;
    let _ = std::mem::size_of::<config::Model>();
    let _ = std::mem::size_of::<reference::Model>();
    let _ = std::mem::size_of::<DbConn>();
    let _ = Set(1i32);

    fn _needs_active_model_trait<T: ActiveModelTrait>() {}
    fn _needs_transaction_trait<T: TransactionTrait>() {}
}

use crate::utils::ignore;

const MAX_BRANCH_NAME_LENGTH: usize = 255;
const LOCK_SUFFIX: &str = ".lock";
const HEAD_REF: &str = "HEAD";
const AT_REF: &str = "@";
const DOT_REF: &str = ".";
const DOUBLE_DOT_REF: &str = "..";
const SLASH: char = '/';
const DOUBLE_SLASH: &str = "//";
const DOUBLE_DOT: &str = "..";

#[derive(thiserror::Error, Debug)]
pub enum InitError {
    #[error("{message}")]
    InvalidArgument {
        message: String,
        hint: Option<String>,
    },

    #[error("source git repository '{path}' does not exist")]
    SourcePathNotFound { path: PathBuf },

    #[error("'{path}' is not a valid Git repository")]
    InvalidGitRepository { path: PathBuf },

    #[error("template directory '{path}' does not exist")]
    TemplateNotFound { path: PathBuf },

    #[error("path '{path}' is not valid UTF-8")]
    InvalidUtf8Path { path: PathBuf },

    #[error("conversion from git repository '{repo}' failed during {stage}: {message}")]
    ConversionFailed {
        repo: PathBuf,
        stage: &'static str,
        message: String,
    },

    #[error("vault initialization failed: {message}")]
    VaultInitializationFailed { message: String },

    #[error("{0}")]
    IgnoreFile(#[from] ignore::IgnoreFileError),

    #[error("{0}")]
    Io(#[from] io::Error),

    #[error("initialization failed due to a storage error: {0}")]
    Database(#[from] DbErr),
}

impl From<InitError> for CliError {
    fn from(error: InitError) -> Self {
        match error {
            InitError::InvalidArgument { message, hint } => {
                // Intent: invalid init flags are user-correctable CLI usage
                // errors, not repository or filesystem failures.
                let mut cli = CliError::command_usage(message)
                    .with_stable_code(StableErrorCode::CliInvalidArguments);
                if let Some(hint) = hint {
                    cli = cli.with_hint(hint);
                }
                cli
            }
            InitError::SourcePathNotFound { path } => {
                // Intent: conversion cannot read the requested source path; the
                // repository state is unchanged, so classify as a read failure.
                CliError::fatal(format!(
                    "source git repository '{}' does not exist",
                    path.display()
                ))
                .with_stable_code(StableErrorCode::IoReadFailed)
            }
            InitError::InvalidGitRepository { path } => CliError::command_usage(format!(
                "'{}' is not a valid Git repository",
                path.display()
            ))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
            .with_hint("a valid Git repository must contain HEAD, config, and objects."),
            InitError::TemplateNotFound { path } => {
                // Intent: `--template` points at a filesystem resource that
                // could not be read; keep the user hint focused on the path.
                CliError::fatal(format!(
                    "template directory '{}' does not exist",
                    path.display()
                ))
                .with_stable_code(StableErrorCode::IoReadFailed)
            }
            InitError::InvalidUtf8Path { path } => {
                CliError::fatal(format!("path '{}' is not valid UTF-8", path.display()))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            }
            InitError::ConversionFailed {
                repo,
                stage,
                message,
            } => {
                // Intent: conversion failures may leave partially initialized
                // repository state, so route agents toward cleanup/retry rather
                // than treating the source Git repository as merely unreadable.
                CliError::fatal(format!(
                    "conversion from git repository '{}' failed during {stage}: {message}",
                    repo.display()
                ))
                .with_stable_code(StableErrorCode::RepoStateInvalid)
            }
            InitError::VaultInitializationFailed { message } => {
                // Intent: vault setup runs after repository metadata exists;
                // failure here means an internal initialization invariant broke
                // and should be reported with enough context for maintainers.
                CliError::fatal(format!("vault initialization failed: {message}"))
                    .with_stable_code(StableErrorCode::InternalInvariant)
                    .with_hint(format!("please report this issue at: {ISSUE_URL}"))
            }
            InitError::IgnoreFile(error) => {
                let stable_code = if error.is_write() {
                    StableErrorCode::IoWriteFailed
                } else {
                    StableErrorCode::IoReadFailed
                };
                CliError::fatal(error.to_string())
                    .with_stable_code(stable_code)
                    .with_hint(error.recovery_hint())
            }
            InitError::Io(error) => match error.kind() {
                io::ErrorKind::InvalidInput => CliError::command_usage(error.to_string())
                    .with_stable_code(StableErrorCode::CliInvalidArguments),
                _ => CliError::fatal(error.to_string())
                    .with_stable_code(StableErrorCode::IoReadFailed),
            },
            InitError::Database(error) => {
                // Intent: schema/bootstrap failures violate the init contract
                // because a newly created repo must always have a usable DB.
                CliError::fatal(format!("database initialization failed: {error}"))
                    .with_stable_code(StableErrorCode::InternalInvariant)
                    .with_hint(format!("please report this issue at: {ISSUE_URL}"))
            }
        }
    }
}

#[derive(ValueEnum, Debug, Clone, PartialEq)]
pub enum RefFormat {
    Strict,
    Filesystem,
}

impl RefFormat {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Filesystem => "filesystem",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InitOutput {
    pub path: String,
    pub bare: bool,
    pub initial_branch: String,
    pub object_format: String,
    pub ref_format: String,
    pub repo_id: String,
    pub vault_signing: bool,
    pub converted_from: Option<String>,
    pub ssh_key_detected: Option<String>,
    pub warnings: Vec<String>,
    /// `true` when this was a Git-style re-initialization of an existing repository
    /// (layout topped-up, database/config/refs preserved) rather than a fresh init.
    #[serde(default)]
    pub reinitialized: bool,
}

#[derive(Parser, Debug, Clone)]
#[command(after_help = EXAMPLES)]
pub struct InitArgs {
    /// Create a bare repository (no working tree; metadata at the target directory itself)
    #[clap(long, required = false)]
    pub bare: bool,

    /// Copy hook and exclude templates from `template-directory` instead of using the built-in defaults
    #[clap(long = "template", name = "template-directory", required = false)]
    pub template: Option<String>,

    /// Override the initial branch name (default: `main`)
    #[clap(short = 'b', long, required = false)]
    pub initial_branch: Option<String>,

    /// Directory in which to create the new `.libra` repository (default: current directory)
    #[clap(value_name = "DIRECTORY", default_value = ".")]
    pub repo_directory: String,

    /// Suppress the "Initialized empty Libra repository" banner (errors still print)
    #[clap(long, short = 'q', required = false)]
    pub quiet: bool,

    /// Filesystem sharing mode: false, umask, group/true, all/world/everybody, or traversable octal like 0770
    #[clap(long, required = false, value_name = "MODE")]
    pub shared: Option<String>,

    /// Object hash algorithm: `sha1` (default) or `sha256`
    #[clap(long = "object-format", name = "format", required = false)]
    pub object_format: Option<String>,

    /// Ref name validation strategy: `strict` (default) or `filesystem`
    #[clap(long = "ref-format", value_enum, required = false)]
    pub ref_format: Option<RefFormat>,

    /// Convert an existing Git repository at `path` into a Libra repository (copies objects, refs, config)
    #[clap(long = "from-git-repository", value_name = "path", required = false)]
    pub from_git_repository: Option<String>,

    /// Initialize the embedded libvault and a PGP signing key (default: true). Pass `--vault false` to skip
    #[clap(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub vault: bool,
}

struct InitProgress {
    enabled: bool,
}

impl InitProgress {
    fn enabled() -> Self {
        Self { enabled: true }
    }

    fn disabled() -> Self {
        Self { enabled: false }
    }

    fn emit(&self, message: impl AsRef<str>) {
        if self.enabled {
            eprintln!("{}", message.as_ref());
        }
    }
}

struct CurrentDirGuard {
    original_dir: PathBuf,
    #[cfg(test)]
    _cwd_lock: crate::utils::test::CwdLockGuard,
}

impl CurrentDirGuard {
    fn change_to(target: &Path) -> io::Result<Self> {
        #[cfg(test)]
        let cwd_lock = crate::utils::test::cwd_lock_guard();
        let original_dir = env::current_dir()?;
        env::set_current_dir(target)?;
        Ok(Self {
            original_dir,
            #[cfg(test)]
            _cwd_lock: cwd_lock,
        })
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        let _ = env::set_current_dir(&self.original_dir);
    }
}

/// Fire-and-forget CLI dispatcher entry for `libra init`.
///
/// # Side Effects
/// - Delegates to [`execute_safe`] with the default [`OutputConfig`].
/// - Prints any rendered [`CliError`] to stderr.
///
/// # Errors
/// This compatibility entry does not return errors. Call [`execute_safe`] when
/// the caller must observe failure details or stable error codes.
pub async fn execute(args: InitArgs) {
    if let Err(error) = execute_safe(args, &OutputConfig::default()).await {
        error.print_stderr();
    }
}

/// Executes repository initialization and renders the requested output format.
///
/// # Side Effects
/// - Creates the target repository storage layout (`.libra/` for non-bare
///   repositories, or the target directory for `--bare`).
/// - Initializes the SQLite database and writes core config plus HEAD/branch
///   reference rows.
/// - Installs default hook and exclude templates unless `--template` supplies
///   replacements.
/// - Creates or updates the root `.libraignore` for non-bare repositories.
/// - Optionally converts objects/refs from an existing Git repository.
/// - Initializes vault credentials and a PGP signing key unless `--vault false`.
/// - Emits human or JSON output according to [`OutputConfig`].
///
/// # Errors
/// Returns a structured [`CliError`] when validation fails, the repository is
/// already initialized, layout/database creation fails, Git conversion fails, or
/// vault/signing setup cannot complete. Stable error-code mapping follows
/// `docs/development/cli-error-contract-design.md`.
pub async fn execute_safe(args: InitArgs, output: &OutputConfig) -> CliResult<()> {
    let mut effective_output = output.clone();
    if args.quiet {
        effective_output.quiet = true;
        effective_output.progress = ProgressMode::None;
        effective_output.progress_preference = crate::utils::output::ProgressPreference::None;
    }

    let progress = if effective_output.is_json() || effective_output.quiet {
        InitProgress::disabled()
    } else {
        InitProgress::enabled()
    };
    let result = run_init_internal(args, &progress).await?;
    render_init_result(&result, &effective_output)
}

fn render_init_result(result: &InitOutput, output: &OutputConfig) -> CliResult<()> {
    // Handle warnings before any early return: record them so `--exit-code-on-warning`
    // fires in every output mode, and (in non-JSON modes) print them to stderr — the
    // `--quiet` contract silences stdout but keeps warnings/errors on stderr. In JSON
    // mode they travel inside the structured payload instead.
    if !result.warnings.is_empty() {
        crate::utils::output::record_warning();
        if !output.is_json() {
            for warning in &result.warnings {
                eprintln!("warning: {warning}");
            }
        }
    }
    if output.is_json() {
        return emit_json_data("init", result, output);
    }
    if output.quiet {
        return Ok(());
    }

    let repo_type = if result.bare { " bare" } else { "" };
    if result.reinitialized {
        println!(
            "Reinitialized existing{repo_type} Libra repository in {}",
            result.path
        );
    } else {
        println!(
            "Initialized empty{repo_type} Libra repository in {}",
            result.path
        );
    }
    println!("  branch: {}", result.initial_branch);
    println!(
        "  signing: {}",
        if result.vault_signing {
            "enabled"
        } else {
            "disabled"
        }
    );

    if !result.vault_signing {
        println!();
        println!("Tip: to enable commit signing later, run: libra config generate-gpg-key");
    }

    println!();
    match &result.ssh_key_detected {
        Some(path) => {
            println!(
                "Tip: using existing SSH key at {}",
                display_home_relative(path)
            );
            println!(
                "     to generate a repo-specific key later, run: libra config generate-ssh-key --remote origin"
            );
        }
        None => {
            println!("Tip: no SSH key found at ~/.ssh/");
            println!("     push/pull via SSH will require a key");
            println!("     generate one with: libra config generate-ssh-key --remote origin");
            println!("     or create a system key: ssh-keygen -t ed25519");
        }
    }

    Ok(())
}

fn display_home_relative(path: &str) -> String {
    let Some(home) = dirs::home_dir() else {
        return path.to_string();
    };
    let home = home.to_string_lossy().to_string();
    if let Some(rest) = path.strip_prefix(&home) {
        return format!("~{rest}");
    }
    path.to_string()
}

/// Runs initialization without rendering.
///
/// # Side Effects
/// Same repository, database, refs, conversion, ignore-file, and vault writes as
/// [`execute_safe`], but no human/JSON success output is emitted.
///
/// # Errors
/// Returns [`InitError`] directly so tests and higher-level commands can assert
/// the domain failure before CLI error mapping.
pub(crate) async fn run_init(args: InitArgs) -> Result<InitOutput, InitError> {
    run_init_internal(args, &InitProgress::disabled()).await
}

#[allow(dead_code)]
/// Legacy initialization helper retained for tests and older call sites.
///
/// # Side Effects
/// Performs the same repository initialization writes as [`run_init`].
///
/// # Errors
/// Returns the underlying [`InitError`] and discards the success metadata.
pub async fn init(args: InitArgs) -> Result<(), InitError> {
    run_init(args).await.map(|_| ())
}

async fn run_init_internal(
    args: InitArgs,
    progress: &InitProgress,
) -> Result<InitOutput, InitError> {
    let current_dir = cur_dir();
    let target_dir = resolve_cli_path(&current_dir, &args.repo_directory);
    let root_dir = storage_root(&target_dir, args.bare);
    let template_dir = args
        .template
        .as_ref()
        .map(|path| resolve_template_path(&current_dir, path))
        .transpose()?;
    let shared_mode = parse_shared_mode(args.shared.as_deref())?;

    if is_reinit(&target_dir, args.bare) {
        // Git-style safe re-initialization: top-up the standard layout and re-apply
        // `--shared`, but PRESERVE the existing database (config, HEAD, refs, objects,
        // vault, repo id). Never recreate config/refs here. Reached BEFORE resolving
        // `--from-git-repository` so that flag is rejected by its raw presence rather
        // than failing first on a missing source path.
        return reinitialize_existing(
            &args,
            &root_dir,
            template_dir.as_deref(),
            shared_mode.as_ref(),
            progress,
        )
        .await;
    }

    let from_git = args
        .from_git_repository
        .as_ref()
        .map(|path| resolve_existing_cli_path(&current_dir, path))
        .transpose()?;
    let object_format = resolve_object_format(args.object_format.as_deref())?;
    let ref_format = args.ref_format.clone().unwrap_or(RefFormat::Strict);
    let initial_branch_name = args
        .initial_branch
        .clone()
        .unwrap_or_else(|| DEFAULT_BRANCH.to_string());

    validate_branch_name(&initial_branch_name, &ref_format)?;

    if target_dir.exists() {
        is_writable(&target_dir)?;
    }

    progress.emit("Creating repository layout ...");
    fs::create_dir_all(&root_dir)?;
    prepare_repository_layout(&root_dir, template_dir.as_deref())?;

    progress.emit("Initializing database ...");
    let database_path = root_dir.join(DATABASE);
    // INVARIANT: the database must exist before refs, config, conversion, or
    // vault setup run; those later stages persist their durable state through
    // this connection/path and assume schema bootstrap has completed.
    let conn = create_database_connection(&database_path).await?;
    // Probe the actual filesystem (root_dir already exists at this point):
    // non-bare repos probe the parent of `.libra`; bare repos have no
    // worktree, so case handling is moot — record false.
    let ignore_case = if args.bare {
        false
    } else {
        root_dir
            .parent()
            .map(crate::utils::path_case::probe_dir_ignore_case)
            .unwrap_or(false)
    };
    let repo_id = init_config(
        &conn,
        args.bare,
        &object_format,
        &ref_format,
        ignore_case,
        shared_mode.as_ref(),
    )
    .await?;

    progress.emit("Setting up refs ...");
    // INVARIANT: refs are initialized after core config so HEAD/branch rows are
    // tied to the repository identity and hash/ref-format choices already stored
    // in config.
    initialize_refs(&conn, &initial_branch_name).await?;

    set_dir_hidden(&root_dir)?;
    if let Some(shared_mode) = shared_mode.as_ref() {
        apply_shared(&root_dir, shared_mode)?;
    }

    let mut warnings = Vec::new();
    if !args.bare {
        ignore::ensure_root_libraignore(&target_dir)?;
    }

    let target_guard_path = target_dir
        .canonicalize()
        .unwrap_or_else(|_| target_dir.clone());

    let converted_from = if let Some(source) = from_git {
        let source_git_dir = convert::resolve_git_source_dir(&source)?;
        progress.emit(format!(
            "Converting from Git repository at {} ...",
            source_git_dir.display()
        ));
        // INVARIANT: conversion helpers read/write paths relative to the target
        // worktree, so the temporary cwd switch must be active for the full
        // conversion call and must be dropped before later stages continue.
        let _guard = CurrentDirGuard::change_to(&target_guard_path)?;
        let report = convert::convert_from_git_repository(&source, args.bare).await?;
        warnings.extend(report.warnings);
        Some(report.source_git_dir)
    } else {
        None
    };

    if args.vault {
        progress.emit("Generating PGP signing key ...");
        // INVARIANT: vault bootstrap runs after DB/config/ref initialization
        // because it records signing state in the repo DB and must roll back its
        // own vault files if credential or key generation fails.
        let _guard = CurrentDirGuard::change_to(&target_guard_path)?;
        init_vault_for_repo(&root_dir, &database_path).await?;
    } else {
        set_vault_signing_value(&database_path, false).await?;
    }

    set_hash_kind(match object_format.as_str() {
        "sha1" => HashKind::Sha1,
        "sha256" => HashKind::Sha256,
        _ => HashKind::Sha1,
    });

    let path = root_dir
        .canonicalize()
        .unwrap_or_else(|_| root_dir.clone())
        .to_string_lossy()
        .to_string();
    Ok(InitOutput {
        path,
        bare: args.bare,
        initial_branch: initial_branch_name,
        object_format,
        ref_format: ref_format.as_str().to_string(),
        repo_id,
        vault_signing: args.vault,
        converted_from,
        ssh_key_detected: detect_system_ssh_key(),
        warnings,
        reinitialized: false,
    })
}

/// Git-style safe re-initialization of an existing repository: top up the standard
/// layout (re-copy templates, ensure directories), re-apply `--shared`, and report
/// the EXISTING identity/format read from the preserved database. The database
/// (config, HEAD, refs, objects, vault, repo id) is never recreated, matching
/// `git init`'s "Reinitialized existing repository" behavior.
async fn reinitialize_existing(
    args: &InitArgs,
    root_dir: &Path,
    template_dir: Option<&Path>,
    shared_mode: Option<&SharedMode>,
    progress: &InitProgress,
) -> Result<InitOutput, InitError> {
    // Validate all flags BEFORE any filesystem side effects, so an invalid invocation
    // leaves the existing repository's layout and permissions untouched (matching
    // fresh init, which validates before mutating).
    //
    // Converting a Git repository into an ALREADY-initialized Libra repository is not
    // supported — conversion must target a fresh directory.
    if args.from_git_repository.is_some() {
        return Err(InitError::InvalidArgument {
            message: "cannot use --from-git-repository on an already-initialized repository"
                .to_string(),
            hint: Some(
                "re-initialization only tops up the existing repository; convert into a fresh directory instead"
                    .to_string(),
            ),
        });
    }
    if let Some(requested) = args.initial_branch.as_deref() {
        let requested_ref_format = args.ref_format.clone().unwrap_or(RefFormat::Strict);
        validate_branch_name(requested, &requested_ref_format)?;
    }
    // Normalize/validate the requested object format once; reused for the warning.
    let requested_object_format = args
        .object_format
        .as_deref()
        .map(|format| resolve_object_format(Some(format)))
        .transpose()?;

    progress.emit("Reinitializing existing repository ...");
    fs::create_dir_all(root_dir)?;
    prepare_repository_layout(root_dir, template_dir)?;
    set_dir_hidden(root_dir)?;
    if let Some(shared_mode) = shared_mode {
        // `apply_shared` skips the vault database/sidecars, so private signing
        // material keeps its owner-only mode through the chmod sweep.
        apply_shared(root_dir, shared_mode)?;
    }

    let database_path = root_dir.join(DATABASE);
    // Connect to the EXISTING database (schema auto-upgrades on open); never recreate
    // it — `create_database_connection` would reject an existing file.
    let conn = get_db_conn_instance_for_path(&database_path)
        .await
        .map_err(InitError::Io)?;
    if let Some(shared_mode) = shared_mode {
        persist_shared_repository(&conn, shared_mode).await?;
    }

    let object_format = read_config_string(&conn, "core.objectformat")
        .await?
        .unwrap_or_else(|| "sha1".to_string());
    let ref_format = read_config_string(&conn, "core.initrefformat")
        .await?
        .unwrap_or_else(|| RefFormat::Strict.as_str().to_string());
    let repo_id = read_config_string(&conn, "libra.repoid")
        .await?
        .unwrap_or_default();
    let bare = read_config_string(&conn, "core.bare")
        .await?
        .map(|value| value == "true")
        .unwrap_or(args.bare);
    let vault_signing = read_config_string(&conn, "vault.signing")
        .await?
        .map(|value| value == "true")
        .unwrap_or(false);
    // Use the fallible HEAD reader so a missing/corrupt HEAD surfaces a structured
    // error rather than panicking.
    let initial_branch = match Head::current_result_with_conn(&conn)
        .await
        .map_err(|error| {
            InitError::Database(DbErr::Custom(format!(
                "failed to read HEAD during re-initialization: {error}"
            )))
        })? {
        Head::Branch(name) => name,
        Head::Detached(_) => "HEAD".to_string(),
    };

    set_hash_kind(match object_format.as_str() {
        "sha256" => HashKind::Sha256,
        _ => HashKind::Sha1,
    });

    // Flags that cannot change an existing repository were validated above; here they
    // are accepted-but-ignored with a warning when they differ from the stored value
    // (Git's tolerant top-up), so the user knows the existing value wins.
    let mut warnings = Vec::new();
    if let Some(requested) = args.initial_branch.as_deref()
        && requested != initial_branch
    {
        warnings.push(format!(
            "ignoring --initial-branch '{requested}' on re-initialization; keeping '{initial_branch}'"
        ));
    }
    if let Some(requested) = &requested_object_format
        && requested != &object_format
    {
        warnings.push(format!(
            "ignoring --object-format '{requested}' on re-initialization; keeping '{object_format}'"
        ));
    }

    let path = root_dir
        .canonicalize()
        .unwrap_or_else(|_| root_dir.to_path_buf())
        .to_string_lossy()
        .to_string();
    Ok(InitOutput {
        path,
        bare,
        initial_branch,
        object_format,
        ref_format,
        repo_id,
        vault_signing,
        converted_from: None,
        ssh_key_detected: detect_system_ssh_key(),
        warnings,
        reinitialized: true,
    })
}

/// Read a single config value from the repository database, mapping store errors to
/// [`InitError::Database`]. Returns `None` when the key is unset.
async fn read_config_string(conn: &DbConn, key: &str) -> Result<Option<String>, InitError> {
    ConfigKv::get_with_conn(conn, key)
        .await
        .map(|entry| entry.map(|entry| entry.value))
        .map_err(|error| InitError::Database(DbErr::Custom(error.to_string())))
}

async fn persist_shared_repository(
    conn: &DbConn,
    shared_mode: &SharedMode,
) -> Result<(), InitError> {
    ConfigKv::set_with_conn(
        conn,
        "core.sharedRepository",
        shared_mode.config_value(),
        false,
    )
    .await
    .map_err(|error| InitError::Database(DbErr::Custom(error.to_string())))
}

fn resolve_cli_path(base: &Path, raw: &str) -> PathBuf {
    let path = Path::new(raw);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn resolve_existing_cli_path(base: &Path, raw: &str) -> Result<PathBuf, InitError> {
    let path = resolve_cli_path(base, raw);
    if !path.exists() {
        return Err(InitError::SourcePathNotFound { path });
    }
    path.canonicalize().map_err(InitError::Io)
}

fn resolve_template_path(base: &Path, raw: &str) -> Result<PathBuf, InitError> {
    let path = resolve_cli_path(base, raw);
    if !path.is_dir() {
        return Err(InitError::TemplateNotFound { path });
    }
    path.canonicalize().map_err(InitError::Io)
}

fn storage_root(target_dir: &Path, bare: bool) -> PathBuf {
    if bare {
        target_dir.to_path_buf()
    } else {
        target_dir.join(ROOT_DIR)
    }
}

fn invalid_argument(message: impl Into<String>, hint: Option<String>) -> InitError {
    InitError::InvalidArgument {
        message: message.into(),
        hint,
    }
}

fn resolve_object_format(raw: Option<&str>) -> Result<String, InitError> {
    let object_format = raw
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_else(|| "sha1".to_string());
    match object_format.as_str() {
        "sha1" | "sha256" => Ok(object_format),
        _ => Err(invalid_argument(
            format!("unsupported object format '{object_format}'"),
            suggest_object_format(&object_format)
                .map(|suggestion| format!("did you mean '{suggestion}'?")),
        )),
    }
}

fn suggest_object_format(value: &str) -> Option<&'static str> {
    (value == "sha265").then_some("sha256")
}

fn is_reinit(target_dir: &Path, bare: bool) -> bool {
    if bare {
        return target_dir.join(DATABASE).exists()
            || target_dir.join("objects").exists()
            || target_dir.join("info").exists()
            || target_dir.join("hooks").exists();
    }
    target_dir.join(ROOT_DIR).exists()
}

fn is_writable(path: &Path) -> io::Result<()> {
    match fs::metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "the target directory is not a directory",
                ));
            }
            if metadata.permissions().readonly() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the target directory is read-only",
                ));
            }
        }
        Err(error) if error.kind() != ErrorKind::NotFound => return Err(error),
        Err(_) => {}
    }
    Ok(())
}

fn prepare_repository_layout(root_dir: &Path, template_dir: Option<&Path>) -> io::Result<()> {
    if let Some(template_dir) = template_dir {
        copy_template(template_dir, root_dir)?;
    } else {
        // Refuse to top up through a symlinked layout directory: writing into it would
        // follow the link and escape the repository. (A legitimately symlinked `.libra`
        // worktree root still contains REAL `info`/`hooks` directories in its shared
        // target, so this only rejects a directly symlinked layout dir.)
        for dir in ["info", "hooks"] {
            let dir_path = root_dir.join(dir);
            refuse_symlinked_layout_path(&dir_path)?;
            fs::create_dir_all(&dir_path)?;
        }
        // Write each standard template only when nothing exists at the destination —
        // `symlink_metadata` does NOT follow links, so a Git-style re-init tops up only
        // truly-missing files: it never clobbers a customized file and never writes
        // THROUGH a symlinked destination (which could escape the repository).
        let exclude_path = root_dir.join("info/exclude");
        if fs::symlink_metadata(&exclude_path).is_err() {
            fs::write(&exclude_path, include_str!("../../template/exclude"))?;
        }
        let pre_commit_sh = root_dir.join("hooks").join("pre-commit.sh");
        if fs::symlink_metadata(&pre_commit_sh).is_err() {
            fs::write(&pre_commit_sh, include_str!("../../template/pre-commit.sh"))?;
            #[cfg(not(target_os = "windows"))]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = fs::Permissions::from_mode(0o755);
                fs::set_permissions(&pre_commit_sh, perms)?;
            }
        }
        let pre_commit_ps1 = root_dir.join("hooks").join("pre-commit.ps1");
        if fs::symlink_metadata(&pre_commit_ps1).is_err() {
            fs::write(
                &pre_commit_ps1,
                include_str!("../../template/pre-commit.ps1"),
            )?;
        }
    }

    // Guard the top-level `objects` dir (and its leaves) against symlinks before
    // creating the pack/info subdirectories.
    refuse_symlinked_layout_path(&root_dir.join("objects"))?;
    for dir in ["objects/pack", "objects/info"] {
        refuse_symlinked_layout_path(&root_dir.join(dir))?;
        fs::create_dir_all(root_dir.join(dir))?;
    }
    Ok(())
}

/// Reject a layout path that is itself a symlink, so layout top-up never follows a
/// link out of the repository. Real directories and truly-absent paths are allowed.
fn refuse_symlinked_layout_path(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            format!(
                "refusing to initialize through symlinked layout path '{}'",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn copy_template(src: &Path, dst: &Path) -> io::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dest_path = dst.join(entry.file_name());

        if file_type.is_dir() {
            // Never recurse through a symlinked destination directory: it would follow
            // the link and write template files outside the repository.
            refuse_symlinked_layout_path(&dest_path)?;
            fs::create_dir_all(&dest_path)?;
            copy_template(&entry.path(), &dest_path)?;
        } else if fs::symlink_metadata(&dest_path).is_err() {
            // `symlink_metadata` does not follow links, so a symlinked destination is
            // left untouched rather than written through; only truly-absent files are
            // copied (Git-style top-up that preserves customized files).
            fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SharedMode {
    False,
    Umask,
    Group,
    All,
    Numeric { raw: String, bits: u32 },
}

impl SharedMode {
    fn parse(raw: &str) -> Result<Self, InitError> {
        match raw {
            "false" => Ok(Self::False),
            "umask" => Ok(Self::Umask),
            "true" | "group" => Ok(Self::Group),
            "all" | "world" | "everybody" => Ok(Self::All),
            mode if mode.starts_with('0') && mode.len() == 4 => {
                let bits = u32::from_str_radix(&mode[1..], 8).map_err(|_| {
                    invalid_argument(
                        format!("invalid shared mode '{mode}'"),
                        Some(
                            "supported numeric shared modes must be 4-digit octal values like 0770"
                                .to_string(),
                        ),
                    )
                })?;
                validate_numeric_shared_mode(mode, bits)?;
                Ok(Self::Numeric {
                    raw: mode.to_string(),
                    bits,
                })
            }
            other => Err(invalid_argument(
                format!("invalid shared mode '{other}'"),
                Some(
                    "supported values: umask, group, all, true, false, or a 4-digit octal mode."
                        .to_string(),
                ),
            )),
        }
    }

    fn config_value(&self) -> &str {
        match self {
            Self::False => "false",
            Self::Umask => "umask",
            Self::Group => "group",
            Self::All => "all",
            Self::Numeric { raw, .. } => raw,
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn permission_bits(&self) -> Option<u32> {
        match self {
            Self::False | Self::Umask => None,
            Self::Group => Some(0o2775),
            Self::All => Some(0o2777),
            Self::Numeric { bits, .. } => Some(*bits),
        }
    }
}

fn parse_shared_mode(shared_mode: Option<&str>) -> Result<Option<SharedMode>, InitError> {
    shared_mode.map(SharedMode::parse).transpose()
}

fn validate_numeric_shared_mode(raw: &str, bits: u32) -> Result<(), InitError> {
    let mut invalid_classes = Vec::new();
    if bits & 0o700 != 0o700 {
        invalid_classes.push("owner rwx");
    }
    if bits & 0o060 != 0 && bits & 0o010 == 0 {
        invalid_classes.push("group execute");
    }
    if bits & 0o006 != 0 && bits & 0o001 == 0 {
        invalid_classes.push("other execute");
    }

    if invalid_classes.is_empty() {
        return Ok(());
    }

    Err(invalid_argument(
        format!(
            "invalid shared mode '{raw}': numeric modes must keep repository directories traversable"
        ),
        Some(format!(
            "missing {}; use a directory-safe mode such as 0770 or 0755",
            invalid_classes.join(", ")
        )),
    ))
}

#[cfg(not(target_os = "windows"))]
fn apply_shared(root_dir: &Path, shared_mode: &SharedMode) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fn set_recursive(dir: &Path, mode: u32) -> io::Result<()> {
        // `WalkDir` does not follow symlinks (so it never descends through them), but
        // `fs::set_permissions` WOULD follow a symlink and chmod its target — which a
        // re-init on a user-populated `.libra` could exploit to touch a path outside
        // the repository. Skip symlinks entirely.
        for entry in walkdir::WalkDir::new(dir) {
            let entry = entry?;
            if entry.file_type().is_symlink() {
                continue;
            }
            let path = entry.path();
            // Never widen the private vault database or its SQLite sidecars: it holds
            // signing material and must stay owner-only even under `--shared
            // group/all` (this also avoids a read window during re-initialization).
            if is_vault_artifact(path) {
                continue;
            }
            let metadata = fs::metadata(path)?;
            let mut perms = metadata.permissions();
            perms.set_mode(mode);
            fs::set_permissions(path, perms)?;
        }
        Ok(())
    }

    if let Some(bits) = shared_mode.permission_bits() {
        set_recursive(root_dir, bits)?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn apply_shared(_root_dir: &Path, _shared_mode: &SharedMode) -> io::Result<()> {
    Ok(())
}

/// True for the vault SQLite database and its sidecars (`vault.db`, `-wal`, `-shm`,
/// `-journal`), which hold private signing material and must never be widened by a
/// `--shared` chmod sweep.
#[cfg(not(target_os = "windows"))]
fn is_vault_artifact(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("vault.db" | "vault.db-wal" | "vault.db-shm" | "vault.db-journal")
    )
}

fn validate_branch_name(branch_name: &str, ref_format: &RefFormat) -> Result<(), InitError> {
    match ref_format {
        RefFormat::Strict => validate_strict_branch_name(branch_name),
        RefFormat::Filesystem => validate_filesystem_branch_name(branch_name),
    }
}

fn validate_strict_branch_name(branch_name: &str) -> Result<(), InitError> {
    if branch_name.is_empty() {
        return Err(invalid_argument("branch name cannot be empty", None));
    }
    if branch_name.len() > MAX_BRANCH_NAME_LENGTH {
        return Err(invalid_argument(
            format!("branch name is too long (max {MAX_BRANCH_NAME_LENGTH} characters)"),
            None,
        ));
    }
    if branch_name == HEAD_REF {
        return Err(invalid_argument("branch name cannot be 'HEAD'", None));
    }
    if branch_name == AT_REF {
        return Err(invalid_argument("branch name cannot be '@'", None));
    }
    if branch_name.chars().any(|c| {
        c.is_control()
            || c == ' '
            || c == '~'
            || c == '^'
            || c == ':'
            || c == '\\'
            || c == '*'
            || c == '['
            || c == '?'
            || c == '"'
            || c == '@'
            || c == '\0'
    }) {
        return Err(invalid_argument(
            format!("branch name contains invalid characters: {branch_name}"),
            None,
        ));
    }
    if branch_name.starts_with(SLASH) || branch_name.ends_with(SLASH) {
        return Err(invalid_argument(
            "branch name cannot start or end with '/'",
            None,
        ));
    }
    if branch_name.contains(DOUBLE_SLASH) {
        return Err(invalid_argument(
            "branch name cannot contain consecutive slashes",
            None,
        ));
    }
    if branch_name.contains(DOUBLE_DOT) {
        return Err(invalid_argument("branch name cannot contain '..'", None));
    }
    if branch_name.ends_with(LOCK_SUFFIX) {
        return Err(invalid_argument(
            "branch name cannot end with '.lock'",
            None,
        ));
    }
    if branch_name.ends_with(DOT_REF) {
        return Err(invalid_argument("branch name cannot end with '.'", None));
    }
    Ok(())
}

fn validate_filesystem_branch_name(branch_name: &str) -> Result<(), InitError> {
    if branch_name.is_empty() {
        return Err(invalid_argument("branch name cannot be empty", None));
    }
    if branch_name.len() > MAX_BRANCH_NAME_LENGTH {
        return Err(invalid_argument(
            format!("branch name is too long (max {MAX_BRANCH_NAME_LENGTH} characters)"),
            None,
        ));
    }
    if branch_name.chars().any(|c| {
        c.is_control()
            || c == '<'
            || c == '>'
            || c == ':'
            || c == '"'
            || c == '|'
            || c == '?'
            || c == '*'
            || c == '\0'
            || (cfg!(windows) && (c == '\\' || c == '/' || c == '\n' || c == '\r'))
    }) {
        return Err(invalid_argument(
            format!("branch name contains filesystem-invalid characters: {branch_name}"),
            None,
        ));
    }
    if branch_name == DOT_REF || branch_name == DOUBLE_DOT_REF {
        return Err(invalid_argument("branch name cannot be '.' or '..'", None));
    }
    Ok(())
}

async fn create_database_connection(database: &Path) -> Result<DbConn, InitError> {
    #[cfg(target_os = "windows")]
    {
        let database = database
            .to_str()
            .ok_or_else(|| InitError::InvalidUtf8Path {
                path: database.to_path_buf(),
            })?
            .replace('\\', "/");
        db::create_database(&database).await.map_err(InitError::Io)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let database = database
            .to_str()
            .ok_or_else(|| InitError::InvalidUtf8Path {
                path: database.to_path_buf(),
            })?;
        db::create_database(database).await.map_err(InitError::Io)
    }
}

async fn initialize_refs(conn: &DbConn, initial_branch_name: &str) -> Result<(), InitError> {
    reference::ActiveModel {
        name: Set(Some(initial_branch_name.to_string())),
        kind: Set(reference::ConfigKind::Head),
        ..Default::default()
    }
    .insert(conn)
    .await?;

    reference::ActiveModel {
        name: Set(Some(crate::internal::branch::INTENT_BRANCH.to_string())),
        kind: Set(reference::ConfigKind::Branch),
        commit: Set(None),
        remote: Set(None),
        ..Default::default()
    }
    .insert(conn)
    .await?;

    // CEX-EntireIO Phase 1.7: register the parallel orphan branch used by the
    // external-agent capture subsystem. Mirrors the `intent` row above; the
    // first checkpoint commit will fill in its `commit` column via the same
    // `HistoryManager::create_append_commit` machinery used by `intent`.
    reference::ActiveModel {
        name: Set(Some(crate::internal::branch::TRACES_BRANCH.to_string())),
        kind: Set(reference::ConfigKind::Branch),
        commit: Set(None),
        remote: Set(None),
        ..Default::default()
    }
    .insert(conn)
    .await?;

    Ok(())
}

async fn init_config(
    conn: &DbConn,
    is_bare: bool,
    object_format: &str,
    ref_format: &RefFormat,
    ignore_case: bool,
    shared_mode: Option<&SharedMode>,
) -> Result<String, DbErr> {
    let txn = conn.begin().await?;

    // `core.ignorecase` is PROBED, not platform-hard-coded (lore.md 1.14):
    // Linux records false, macOS records what the volume actually is, and a
    // case-sensitive NTFS volume on Windows records false too.
    let ignorecase_text = if ignore_case { "true" } else { "false" };
    #[cfg(not(target_os = "windows"))]
    let entries = [
        ("repositoryformatversion", "0"),
        ("filemode", "true"),
        ("bare", if is_bare { "true" } else { "false" }),
        ("logallrefupdates", "true"),
        ("ignorecase", ignorecase_text),
    ];

    #[cfg(target_os = "windows")]
    let entries = [
        ("repositoryformatversion", "0"),
        ("filemode", "false"),
        ("bare", if is_bare { "true" } else { "false" }),
        ("logallrefupdates", "true"),
        ("symlinks", "false"),
        ("ignorecase", ignorecase_text),
    ];

    let repo_id = uuid::Uuid::new_v4().to_string();

    for (key, value) in &entries {
        ConfigKv::set_with_conn(&txn, &format!("core.{key}"), value, false)
            .await
            .map_err(|error| DbErr::Custom(error.to_string()))?;
    }
    ConfigKv::set_with_conn(&txn, "core.objectformat", object_format, false)
        .await
        .map_err(|error| DbErr::Custom(error.to_string()))?;
    ConfigKv::set_with_conn(&txn, "core.initrefformat", ref_format.as_str(), false)
        .await
        .map_err(|error| DbErr::Custom(error.to_string()))?;
    ConfigKv::set_with_conn(&txn, "libra.repoid", &repo_id, false)
        .await
        .map_err(|error| DbErr::Custom(error.to_string()))?;
    if let Some(shared_mode) = shared_mode {
        ConfigKv::set_with_conn(
            &txn,
            "core.sharedRepository",
            shared_mode.config_value(),
            false,
        )
        .await
        .map_err(|error| DbErr::Custom(error.to_string()))?;
    }

    txn.commit().await?;
    Ok(repo_id)
}

#[cfg(target_os = "windows")]
fn set_dir_hidden(dir: &Path) -> io::Result<()> {
    use std::process::Command;

    let dir = dir.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("path '{}' is not valid UTF-8", dir.display()),
        )
    })?;
    Command::new("attrib").arg("+H").arg(dir).spawn()?.wait()?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn set_dir_hidden(_dir: &Path) -> io::Result<()> {
    Ok(())
}

async fn init_vault_for_repo(root_dir: &Path, database_path: &Path) -> Result<(), InitError> {
    use crate::internal::vault;

    let identity_sources =
        resolve_user_identity_sources(LocalIdentityTarget::ExplicitDb(database_path))
            .await
            .map_err(|error| InitError::VaultInitializationFailed {
                message: format!("{error:#}"),
            })?;
    let user_name = identity_sources
        .config_name
        .or(identity_sources.env_name)
        .unwrap_or_else(|| "Libra User".to_string());
    let user_email = identity_sources
        .config_email
        .or(identity_sources.env_email)
        .unwrap_or_else(|| "user@libra.local".to_string());

    let (unseal_key, enc_token) = vault::init_vault(root_dir).await.map_err(|error| {
        InitError::VaultInitializationFailed {
            message: format!("{error:#}"),
        }
    })?;

    if let Err(error) = vault::store_credentials(&unseal_key, &enc_token).await {
        rollback_failed_vault_init(root_dir).await;
        return Err(InitError::VaultInitializationFailed {
            message: format!("{error:#}"),
        });
    }

    if let Err(error) =
        vault::generate_pgp_key(root_dir, &unseal_key, &user_name, &user_email).await
    {
        rollback_failed_vault_init(root_dir).await;
        return Err(InitError::VaultInitializationFailed {
            message: format!("{error:#}"),
        });
    }

    set_vault_signing_value(database_path, true).await
}

async fn set_vault_signing_value(database_path: &Path, enabled: bool) -> Result<(), InitError> {
    let conn = get_db_conn_instance_for_path(database_path)
        .await
        .map_err(InitError::Io)?;
    ConfigKv::set_with_conn(
        &conn,
        "vault.signing",
        if enabled { "true" } else { "false" },
        false,
    )
    .await
    .map_err(|error| InitError::VaultInitializationFailed {
        message: format!("{error:#}"),
    })
}

async fn rollback_failed_vault_init(root_dir: &Path) {
    use crate::internal::vault;

    vault::remove_credentials().await;

    for suffix in ["", "-wal", "-shm"] {
        let path = root_dir.join(format!("vault.db{suffix}"));
        if let Err(error) = fs::remove_file(&path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(
                "failed to remove partially initialized vault database '{}': {}",
                path.display(),
                error
            );
        }
    }
}

fn detect_system_ssh_key() -> Option<String> {
    let home = dirs::home_dir()?;
    let ssh_dir = home.join(".ssh");
    for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
        let path = ssh_dir.join(name);
        if path.exists() {
            return Some(path.to_string_lossy().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        path::PathBuf,
    };

    use gag::BufferRedirect;
    use serial_test::serial;
    use tempfile::tempdir;

    use super::{DEFAULT_BRANCH, InitArgs, InitError, run_init};
    use crate::utils::test::{self, ChangeDirGuard};

    #[test]
    fn init_error_display_pins_owned_variants() {
        assert_eq!(
            InitError::InvalidArgument {
                message: "missing target".to_string(),
                hint: Some("provide a path".to_string()),
            }
            .to_string(),
            "missing target",
        );
        assert_eq!(
            InitError::SourcePathNotFound {
                path: PathBuf::from("/missing/repo"),
            }
            .to_string(),
            "source git repository '/missing/repo' does not exist",
        );
        assert_eq!(
            InitError::InvalidGitRepository {
                path: PathBuf::from("/tmp/not-git"),
            }
            .to_string(),
            "'/tmp/not-git' is not a valid Git repository",
        );
        assert_eq!(
            InitError::TemplateNotFound {
                path: PathBuf::from("/tmp/template"),
            }
            .to_string(),
            "template directory '/tmp/template' does not exist",
        );
        assert_eq!(
            InitError::InvalidUtf8Path {
                path: PathBuf::from("/tmp/utf8"),
            }
            .to_string(),
            "path '/tmp/utf8' is not valid UTF-8",
        );
        assert_eq!(
            InitError::ConversionFailed {
                repo: PathBuf::from("/tmp/source"),
                stage: "objects",
                message: "missing pack".to_string(),
            }
            .to_string(),
            "conversion from git repository '/tmp/source' failed during objects: missing pack",
        );
        assert_eq!(
            InitError::VaultInitializationFailed {
                message: "no keyring".to_string(),
            }
            .to_string(),
            "vault initialization failed: no keyring",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn run_init_is_silent_for_internal_callers() {
        let repo = tempdir().expect("failed to create temp repo");
        test::setup_clean_testing_env_in(repo.path());
        let _guard = ChangeDirGuard::new(repo.path());

        let mut stdout = BufferRedirect::stdout().expect("failed to redirect stdout");
        let mut stderr = BufferRedirect::stderr().expect("failed to redirect stderr");

        let result = run_init(InitArgs {
            bare: false,
            template: None,
            initial_branch: None,
            repo_directory: ".".to_string(),
            quiet: false,
            shared: None,
            object_format: None,
            ref_format: None,
            from_git_repository: None,
            vault: false,
        })
        .await
        .expect("run_init should succeed without rendering side effects");

        std::io::stdout()
            .flush()
            .expect("failed to flush captured stdout");
        std::io::stderr()
            .flush()
            .expect("failed to flush captured stderr");

        let mut captured_stdout = String::new();
        stdout
            .read_to_string(&mut captured_stdout)
            .expect("failed to read captured stdout");

        let mut captured_stderr = String::new();
        stderr
            .read_to_string(&mut captured_stderr)
            .expect("failed to read captured stderr");

        assert_eq!(result.initial_branch, DEFAULT_BRANCH);
        assert!(!result.vault_signing);
        assert!(
            !captured_stdout.contains("Initialized empty ")
                && !captured_stdout.contains("branch: ")
                && !captured_stdout.contains("signing: "),
            "run_init must not render init summary to stdout for internal callers, got: {captured_stdout:?}"
        );
        assert!(
            !captured_stderr.contains("Creating repository layout ...")
                && !captured_stderr.contains("Initializing database ...")
                && !captured_stderr.contains("Setting up refs ...")
                && !captured_stderr.contains("Converting from Git repository")
                && !captured_stderr.contains("Generating PGP signing key ..."),
            "run_init must not render init progress to stderr for internal callers, got: {captured_stderr:?}"
        );
    }
}
