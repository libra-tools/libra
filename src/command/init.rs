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
use git_internal::hash::set_hash_kind;
use sea_orm::{ActiveModelTrait, DbConn, DbErr, Set, TransactionTrait};
use serde::Serialize;

use crate::{
    command::init_conversion,
    internal::{
        config::{
            ConfigKv, LocalIdentityTarget, read_cascaded_config_value_strict,
            resolve_user_identity_sources,
        },
        db::{self, get_db_conn_instance_for_path},
        head::Head,
        model::{config, reference},
    },
    utils::{
        convert,
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, ProgressMode, emit_json_data},
        util::{DATABASE, ROOT_DIR, cur_dir, is_global_libra_home},
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
    libra init --object-format sha256          Use SHA-256 hashing
    libra init --object-format blake3          Use BLAKE3 hashing"#;

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
    /// A retry of an interrupted conversion was refused with a specific
    /// stable code (ADR-IG-03 item 2).
    #[error("{message}")]
    ConversionRetryRefused {
        code: StableErrorCode,
        message: String,
        hint: String,
    },

    #[error("{message}")]
    InvalidArgument {
        message: String,
        hint: Option<String>,
    },

    #[error("failed to read config '{key}': {detail}")]
    ConfigRead { key: String, detail: String },

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

    /// Convert/object-format combination that must fail closed before any write.
    #[error("{message}")]
    ConvertObjectFormatForbidden {
        message: String,
        hint: Option<String>,
    },

    /// Reinit with a different object format: Git refuses loudly (#473).
    #[error("attempt to reinitialize repository with different hash")]
    ReinitObjectFormatMismatch { stored: String, requested: String },

    /// A storage path exists but is not a directory (#473 IN-03).
    #[error("'{}' exists but is not a directory", path.display())]
    StoragePathNotDirectory { path: PathBuf },

    /// Stored or source `core.objectformat` / Git extensions.objectformat is unknown.
    #[error("{message}")]
    UnknownObjectFormat { message: String },

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
            InitError::ConversionRetryRefused {
                code,
                message,
                hint,
            } => CliError::fatal(message)
                .with_stable_code(code)
                .with_hint(hint),
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
            InitError::ConfigRead { key, detail } => CliError::fatal(format!(
                "failed to read config '{key}': {detail}"
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)
            .with_hint(
                "pass the value explicitly to bypass this config lookup (e.g. 'libra init -b <name>' \
                 for the initial branch), or repair the config database named in the error above; \
                 'libra config --global' only changes the global config database and cannot \
                 repair a repository's local database"
                    .to_string(),
            ),
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
            InitError::ConvertObjectFormatForbidden { message, hint } => {
                let mut cli = CliError::command_usage(message)
                    .with_stable_code(StableErrorCode::CliInvalidArguments);
                if let Some(hint) = hint {
                    cli = cli.with_hint(hint);
                }
                cli
            }
            InitError::ReinitObjectFormatMismatch { stored, requested } => {
                // Intent: repository state conflict, not CLI usage — Git exits
                // 128 here; scripts keying on the exit code must see a repo
                // failure, matching ADR-IN-02.
                CliError::fatal(format!(
                    "attempt to reinitialize repository with different hash: stored '{stored}', requested '{requested}'"
                ))
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint(
                    "create a new repository with `libra init --object-format <format>` instead"
                        .to_string(),
                )
            }
            InitError::StoragePathNotDirectory { path } => {
                // Intent: the target is a pre-existing file where a directory
                // is required; fail before any layout or database write.
                CliError::fatal(format!(
                    "'{}' exists but is not a directory",
                    path.display()
                ))
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint(
                    "remove or relocate the file, or choose a different target directory"
                        .to_string(),
                )
            }
            InitError::UnknownObjectFormat { message } => CliError::fatal(message)
                .with_stable_code(StableErrorCode::RepoCorrupt),
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
                io::ErrorKind::NotADirectory => CliError::fatal(error.to_string())
                    .with_stable_code(StableErrorCode::RepoCorrupt)
                    .with_hint(
                        "choose a target path whose existing components are directories"
                            .to_string(),
                    ),
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
    #[serde(skip)]
    pub(crate) storage_path: PathBuf,
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

    /// Override the initial branch name (default: `init.defaultBranch`, then `main`)
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

    /// Object hash algorithm: `sha1` (default), `sha256`, or `blake3`
    #[clap(long = "object-format", name = "format", required = false)]
    pub object_format: Option<String>,

    /// Ref name validation strategy: `strict` (default) or `filesystem`. Git's
    /// `files`/`reftable` backends are not implemented and are refused with a hint
    #[clap(long = "ref-format", value_name = "FORMAT", required = false)]
    pub ref_format: Option<String>,

    /// Git `--separate-git-dir` layout (not implemented; refused with a migration hint)
    #[clap(long = "separate-git-dir", value_name = "PATH", required = false)]
    pub separate_git_dir: Option<String>,

    /// Libra's legacy `--separate-libra-dir` layout (not implemented; refused)
    #[clap(long = "separate-libra-dir", value_name = "PATH", required = false)]
    pub separate_libra_dir: Option<String>,

    /// Git `--recurse-submodules` init (not implemented; refused with a hint)
    #[clap(long = "recurse-submodules", required = false)]
    pub recurse_submodules: bool,

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
    execute_safe_with_storage(args, output).await.map(|_| ())
}

/// Return the storage path actually used by initialization so CLI post-processing
/// does not reopen a different path spelling after a symlink or `..` alias.
pub(crate) async fn execute_safe_with_storage(
    args: InitArgs,
    output: &OutputConfig,
) -> CliResult<PathBuf> {
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
    render_init_result(&result, &effective_output)?;
    Ok(result.storage_path)
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

/// Legacy initialization helper retained for tests and older call sites.
///
/// # Side Effects
/// Performs the same repository initialization writes as `run_init`.
///
/// # Errors
/// Returns the underlying [`InitError`] and discards the success metadata.
pub async fn init(args: InitArgs) -> Result<(), InitError> {
    run_init(args).await.map(|_| ())
}

async fn resolve_initial_branch_name(
    cli_value: Option<&str>,
    ref_format: &RefFormat,
) -> Result<String, InitError> {
    if let Some(branch) = cli_value {
        return Ok(branch.to_string());
    }

    match read_cascaded_config_value_strict(LocalIdentityTarget::CurrentRepo, "init.defaultBranch")
        .await
    {
        Ok(Some(branch)) => {
            if let Err(error) = validate_branch_name(&branch, ref_format) {
                return Err(InitError::InvalidArgument {
                    message: format!("invalid init.defaultBranch value '{branch}': {error}"),
                    hint: Some(
                        "set a valid branch with 'libra config --global init.defaultBranch <name>' or pass --initial-branch"
                            .to_string(),
                    ),
                });
            }
            Ok(branch)
        }
        Ok(None) => Ok(DEFAULT_BRANCH.to_string()),
        Err(error) => Err(InitError::ConfigRead {
            key: "init.defaultBranch".to_string(),
            detail: format!("{error:#}"),
        }),
    }
}

/// Resolve the conversion retry BEFORE the central operation boundary runs.
///
/// A half-converted repository has no `libra.repoid`, so the operation-log
/// layer cannot open it; the retry must therefore move the residue aside in
/// the CLI preflight, exactly like the in-command path does for a target the
/// operation layer can still open (ADR-IG-03 item 2). Returns the aside path
/// when a residue was moved.
pub fn preflight_conversion_retry(args: &InitArgs) -> Result<Option<PathBuf>, InitError> {
    let Some(raw_source) = args.from_git_repository.as_deref() else {
        return Ok(None);
    };
    let current_dir = cur_dir();
    let target_dir = resolve_init_target(&current_dir, &args.repo_directory)?;
    let root_dir = storage_root(&target_dir, args.bare);
    if init_conversion::read_marker(&root_dir).is_none() {
        return Ok(None);
    }
    // The marker exists: the same validation the command path performs decides
    // whether this run may adopt the target.
    let source = resolve_existing_cli_path(&current_dir, raw_source)?;
    let canonical_source = convert::resolve_git_source_dir(&source)?;
    match init_conversion::decide_retry(&root_dir, &canonical_source) {
        init_conversion::RetryDecision::Proceed => Ok(None),
        init_conversion::RetryDecision::MoveAside(aside) => init_conversion::move_aside(&root_dir)
            .map_err(|error| {
                InitError::Io(std::io::Error::other(format!(
                    "cannot move the interrupted conversion aside to '{}': {error}",
                    aside.display()
                )))
            })
            .map(Some),
        init_conversion::RetryDecision::Refuse {
            code,
            message,
            hint,
        } => Err(InitError::ConversionRetryRefused {
            code,
            message,
            hint,
        }),
    }
}

async fn run_init_internal(
    args: InitArgs,
    progress: &InitProgress,
) -> Result<InitOutput, InitError> {
    // §C.4.2: `init` BUILDS the repository it then steps into, so it acts on
    // no pre-existing worktree. Run unpinned — a pin taken in an enclosing
    // repository would make the pinned resolvers write the ENCLOSING
    // repository's index/HEAD while this command is inside the new one.
    let _scope = crate::internal::worktree_scope::WorktreeScope::unpinned();
    let current_dir = cur_dir();
    let target_dir = resolve_init_target(&current_dir, &args.repo_directory)?;
    let root_dir = storage_root(&target_dir, args.bare);

    // Git layout/storage options that are documented intentional differences
    // (ADR-IN-07/ADR-IN-08, D25): refuse at the parse boundary with a migration
    // hint instead of the generic clap "unexpected argument" message.
    if args.separate_git_dir.is_some() || args.separate_libra_dir.is_some() {
        return Err(invalid_argument(
            "--separate-git-dir/--separate-libra-dir is not implemented; Libra stores the \
             repository database inside the target's own '.libra/'"
                .to_string(),
            Some(
                "migrate a Git repository that uses a gitdir pointer with \
                 'libra init --from-git-repository <worktree>'"
                    .to_string(),
            ),
        ));
    }
    if args.recurse_submodules {
        return Err(invalid_argument(
            "--recurse-submodules is not implemented; submodule init is a separate step"
                .to_string(),
            Some("run 'libra submodule init' after creating the repository".to_string()),
        ));
    }
    let ref_format = resolve_ref_format(args.ref_format.as_deref())?;

    // Empty `--template=` means "no template": only the repository-required
    // structure is created, and the built-in hooks/exclude defaults are skipped.
    let template_dir = match args.template.as_deref() {
        Some("") => None,
        Some(path) => Some(resolve_template_path(&current_dir, path)?),
        // Boxed: keeps the init future's layout depth bounded for callers.
        None => Box::pin(resolve_template_from_sources()).await?,
    };
    // Self-reference guard (ADR-IN-03, issue #501): a template that is the
    // storage root or the target worktree itself cannot be copied into that
    // same worktree, and an ANCESTOR template must not descend into the target
    // being created — otherwise the copy recurses forever (`--template=.` used
    // to nest `<dir>/.libra/<dir>/.libra/...` until ENAMETOOLONG).
    //
    // The skip is the TARGET worktree, not just the storage root: the storage
    // root (`<target>/.libra`) is inside the target, so skipping only the
    // latter still let the walker copy `<target>` itself.
    if let Some(template) = template_dir.as_deref() {
        // `resolve_template_path` is canonical, while CLI target spellings can
        // contain symlinks or `..`. Compare real paths before any target write.
        let target = resolve_template_target_path(&target_dir).map_err(|source| {
            InitError::Io(io::Error::new(
                source.kind(),
                format!(
                    "failed to resolve target repository '{}' for template safety: {source}",
                    target_dir.display()
                ),
            ))
        })?;
        let root = resolve_template_target_path(&root_dir).map_err(|source| {
            InitError::Io(io::Error::new(
                source.kind(),
                format!(
                    "failed to resolve target storage '{}' for template safety: {source}",
                    root_dir.display()
                ),
            ))
        })?;
        if template == target || template == root {
            return Err(invalid_argument(
                format!(
                    "template directory '{}' is the target repository itself",
                    template.display()
                ),
                Some("use the parent directory as --template, or pick another target".to_string()),
            ));
        }
        if template.starts_with(&root) {
            return Err(invalid_argument(
                format!(
                    "template directory '{}' is inside the target storage directory '{}'",
                    template.display(),
                    root.display()
                ),
                Some("choose a template directory outside the repository storage".to_string()),
            ));
        }
    }
    let shared_mode = parse_shared_mode(args.shared.as_deref())?;

    // The Libra home (`$LIBRA_HOME`, default `~/.libra`) holds per-user state
    // — global config (`config.db`), vault keys, binaries. A repository whose
    // storage root IS the home would mix repo data into that state, so refuse
    // before the reinit check (an existing home `libra.db` artifact would
    // otherwise be "reinitialized" as a repository).
    if is_global_libra_home(&root_dir) {
        return Err(invalid_argument(
            format!(
                "refusing to initialize a Libra repository at '{}': this is a per-user Libra \
                 state directory (the Libra home or the global configuration directory), which \
                 stores global configuration and per-user state; repository data must live in a \
                 dedicated project directory's '.libra/'",
                root_dir.display()
            ),
            Some("choose a project subdirectory, e.g. 'libra init <project>'".to_string()),
        ));
    }

    // Storage path type preflight (ADR-IN-02 IN-03): a pre-existing FILE where
    // a storage directory is required must fail before the reinit check and any
    // write, with LBR-REPO-002/128 rather than a misleading "Reinitializing".
    ensure_storage_paths_are_directories(&target_dir, args.bare)?;

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
            &target_dir,
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
    // ADR-IG-03 item 2: a previous conversion that was hard-killed leaves a
    // marker. Decide BEFORE any write whether this run may adopt that target.
    let mut conversion_residue: Option<PathBuf> = None;
    if let Some(source) = from_git.as_deref() {
        let canonical_source = convert::resolve_git_source_dir(source)?;
        match init_conversion::decide_retry(&root_dir, &canonical_source) {
            init_conversion::RetryDecision::Proceed => {}
            init_conversion::RetryDecision::MoveAside(aside) => {
                // Never delete the residue: it may hold user-owned data. Moving
                // it aside makes the target retryable and reports where it went.
                init_conversion::move_aside(&root_dir).map_err(|error| {
                    InitError::Io(std::io::Error::other(format!(
                        "cannot move the interrupted conversion aside to '{}': {error}",
                        aside.display()
                    )))
                })?;
                conversion_residue = Some(aside);
            }
            init_conversion::RetryDecision::Refuse {
                code,
                message,
                hint,
            } => {
                return Err(InitError::ConversionRetryRefused {
                    code,
                    message,
                    hint,
                });
            }
        }
    }
    if let Some(source) = from_git.as_deref() {
        // Conversion validation must precede every target write. Keep the same
        // validation inside the converter as defense in depth for direct callers.
        convert::validate_source_head(source)?;
    }
    let is_git_conversion = from_git.is_some();
    // Boxed: keeps the init future's layout depth bounded for callers.
    let object_format = Box::pin(resolve_object_format_from_sources(
        args.object_format.as_deref(),
    ))
    .await?;
    // ADR-B3-01: Convert is SHA-1→SHA-1 only. Refuse sha256/blake3 targets (and
    // sha256 Git sources) before create_dir_all / layout / DB.
    if let Some(source) = from_git.as_deref() {
        refuse_convert_object_format_combination(source, &object_format)?;
    }
    let initial_branch_name = if is_git_conversion {
        // Git conversion selects HEAD from the source repository's branch layout;
        // use a harmless placeholder until `clone::setup_repository` installs it.
        DEFAULT_BRANCH.to_string()
    } else {
        resolve_initial_branch_name(args.initial_branch.as_deref(), &ref_format).await?
    };

    validate_branch_name(&initial_branch_name, &ref_format)?;

    if target_dir.exists() {
        is_writable(&target_dir)?;
    }

    let target_dir_existed = target_dir.exists();
    // `resolve_init_target` already resolved every existing component. Keep
    // this path identical to the one used for ownership classification.
    let target_guard_path = target_dir.clone();
    // A bare repository uses the target itself as storage. If the target
    // already exists, rollback must retain its user-owned entries and remove
    // only repository entries created by this conversion.
    let bare_snapshot = if is_git_conversion && args.bare && target_dir_existed {
        let mut reserved = vec![
            DATABASE.to_string(),
            format!("{DATABASE}-wal"),
            format!("{DATABASE}-shm"),
            format!("{DATABASE}-journal"),
        ];
        if args.vault {
            reserved.extend(
                [
                    "vault.db",
                    "vault.db-wal",
                    "vault.db-shm",
                    "vault.db-journal",
                ]
                .map(str::to_string),
            );
        }
        for name in reserved {
            match fs::symlink_metadata(root_dir.join(&name)) {
                Ok(_) => {
                    return Err(invalid_argument(
                        format!(
                            "cannot convert into bare target '{}': repository file '{}' already exists",
                            root_dir.display(),
                            name
                        ),
                        Some(
                            "choose an empty bare target or move the conflicting file aside"
                                .to_string(),
                        ),
                    ));
                }
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(InitError::Io(error)),
            }
        }
        let mut layout_names = ["objects", "info", "hooks"]
            .into_iter()
            .map(std::ffi::OsString::from)
            .collect::<std::collections::HashSet<_>>();
        if let Some(template) = template_dir.as_deref() {
            for entry in fs::read_dir(template)? {
                layout_names.insert(entry?.file_name());
            }
        }
        let mut missing_layout_names = std::collections::HashSet::new();
        for name in layout_names {
            match fs::symlink_metadata(root_dir.join(&name)) {
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    missing_layout_names.insert(name);
                }
                Err(error) => return Err(InitError::Io(error)),
            }
        }
        Some(BareTargetSnapshot {
            missing_layout_names,
        })
    } else {
        None
    };
    progress.emit("Creating repository layout ...");
    let root_parent = root_dir.parent().ok_or_else(|| {
        InitError::Io(io::Error::new(
            ErrorKind::InvalidInput,
            format!(
                "target storage '{}' has no parent directory",
                root_dir.display()
            ),
        ))
    })?;
    fs::create_dir_all(root_parent)?;
    // Only a successful, exclusive create gives this invocation ownership of
    // the storage directory. A path with a missing/.. segment can look absent
    // before parent creation yet resolve to an existing user directory after.
    let root_created = match fs::create_dir(&root_dir) {
        Ok(()) => true,
        Err(error) if error.kind() == ErrorKind::AlreadyExists && root_dir.is_dir() => false,
        Err(error) => return Err(InitError::Io(error)),
    };
    if is_git_conversion && !root_created && bare_snapshot.is_none() {
        return Err(InitError::Io(io::Error::new(
            ErrorKind::AlreadyExists,
            format!(
                "target storage '{}' appeared during initialization; retry after checking its contents",
                root_dir.display()
            ),
        )));
    }
    if let Err(error) = prepare_repository_layout(
        &root_dir,
        &target_dir,
        template_dir.as_deref(),
        args.template.as_deref() != Some(""),
    ) {
        // ADR-IN-03 item 6: a fresh-repo template/layout failure rolls back the
        // storage directory this run created, restoring the pre-run snapshot.
        if root_created {
            let _ = fs::remove_dir_all(&root_dir);
        } else if let Some(snapshot) = bare_snapshot.as_ref()
            && let Err(cleanup) = rollback_bare_layout(&root_dir, snapshot)
        {
            return Err(InitError::Io(io::Error::other(format!(
                "failed to initialize bare repository '{}': {error}; could not clean up this run's layout: {cleanup}",
                root_dir.display()
            ))));
        }
        return Err(InitError::Io(error));
    }

    // ADR-IG-02: the marker lands before any further target write and is
    // removed as the final write of a successful conversion. Everything from
    // here to the end of this function is rollback-protected: an ordinary
    // error, and (via the listener below) SIGINT/SIGTERM, undo exactly what
    // this run created.
    let mut conversion_guard = match from_git.as_deref() {
        Some(source) => {
            let canonical_source = convert::resolve_git_source_dir(source)?;
            let mut guard = init_conversion::ConversionGuard::begin(&root_dir, &canonical_source)
                .map_err(|error| {
                InitError::Io(std::io::Error::other(format!(
                    "cannot record the Git conversion marker in '{}': {error}",
                    root_dir.display()
                )))
            })?;
            if let Some(snapshot) = bare_snapshot.as_ref() {
                // Layout ran before the marker was installed. Record every
                // new top-level entry and the database files that later stages
                // may create; never record the pre-existing directory itself.
                for (path, directory) in new_bare_layout_entries(&root_dir, snapshot)? {
                    guard.record_created(&path, directory)?;
                }
                for name in [
                    DATABASE.to_string(),
                    format!("{DATABASE}-wal"),
                    format!("{DATABASE}-shm"),
                    format!("{DATABASE}-journal"),
                ] {
                    guard.record_created(&root_dir.join(name), false)?;
                }
                if args.vault {
                    for name in [
                        "vault.db",
                        "vault.db-wal",
                        "vault.db-shm",
                        "vault.db-journal",
                    ] {
                        guard.record_created(&root_dir.join(name), false)?;
                    }
                }
            } else if target_dir_existed && root_created {
                guard.record_created(&root_dir, true)?;
                let root_ignore = target_guard_path.join(".libraignore");
                if !root_ignore.exists() {
                    guard.record_created(&root_ignore, false)?;
                }
            } else if !target_dir_existed && root_created {
                guard.record_created(&target_guard_path, true)?;
            }
            init_conversion::install_signal_rollback(root_dir.clone());
            Some(guard)
        }
        None => None,
    };

    // Everything below runs under the conversion guard: an ordinary error (or a
    // signal, via `install_signal_rollback`) rolls this run's target state back
    // so the conversion can be retried immediately (ADR-IG-02/03).
    // Keep a handle to the new pool until an error has been handled. Dropping
    // the connection alone can leave SQLite files open during Windows rollback.
    #[cfg(windows)]
    let mut conversion_db_for_rollback: Option<DbConn> = None;
    let init_result: Result<InitOutput, InitError> = async {
        progress.emit("Initializing database ...");
        let database_path = root_dir.join(DATABASE);
        // INVARIANT: the database must exist before refs, config, conversion, or
        // vault setup run; those later stages persist their durable state through
        // this connection/path and assume schema bootstrap has completed.
        let conn = create_database_connection(&database_path).await?;
        #[cfg(windows)]
        if is_git_conversion {
            conversion_db_for_rollback = Some(conn.clone());
        }
        conversion_gate(init_conversion::ConversionStage::AfterDatabase)?;
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
            if let Some(snapshot) = bare_snapshot.as_ref() {
                apply_shared_bare_created(&root_dir, snapshot, shared_mode)?;
            } else {
                apply_shared(&root_dir, shared_mode)?;
            }
        }

        let mut warnings = Vec::new();
        // WT-03 (ADR-WT-04): do NOT create a comment-only root `.libraignore`.
        // Git leaves a fresh worktree clean, and the stub carried no rules; a
        // user-created `.libraignore` (or one produced by `.gitignore`
        // conversion below) still works. The former `ensure_root_libraignore`
        // call is intentionally gone so `libra init` matches Git's post-init UX.

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
            conversion_gate(init_conversion::ConversionStage::AfterSetup)?;
            warnings.extend(report.warnings);
            Some(report.source_git_dir)
        } else {
            None
        };

        let initial_branch_name = if converted_from.is_some() {
            let _guard = CurrentDirGuard::change_to(&target_guard_path)?;
            match Head::current_result().await.map_err(|error| {
                InitError::Database(DbErr::Custom(format!(
                    "failed to read converted HEAD: {error}"
                )))
            })? {
                Head::Branch(name) => name,
                Head::Detached(_) => "HEAD".to_string(),
            }
        } else {
            initial_branch_name
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

        set_hash_kind(
            crate::internal::object_format::parse_config_value(&object_format).map_err(|_| {
                InitError::UnknownObjectFormat {
                    message: format!("unsupported object format '{object_format}'"),
                }
            })?,
        );

        let path = root_dir
            .canonicalize()
            .unwrap_or_else(|_| root_dir.clone())
            .to_string_lossy()
            .to_string();
        Ok(InitOutput {
            path,
            storage_path: root_dir.clone(),
            bare: args.bare,
            initial_branch: initial_branch_name,
            ref_format: ref_format.as_str().to_string(),
            object_format,
            repo_id,
            vault_signing: args.vault,
            converted_from,
            ssh_key_detected: detect_system_ssh_key(),
            warnings,
            reinitialized: false,
        })
    }
    .await;

    match init_result {
        Ok(mut output) => {
            if let Some(guard) = conversion_guard.take() {
                guard.finish().map_err(|error| {
                    InitError::Io(std::io::Error::other(format!(
                        "cannot clear the Git conversion marker in '{}': {error}",
                        root_dir.display()
                    )))
                })?;
            }
            if let Some(aside) = conversion_residue.as_deref() {
                output.warnings.push(format!(
                    "the interrupted conversion was moved aside to '{}' (review it, then delete it when it holds nothing you need)",
                    aside.display()
                ));
            }
            Ok(output)
        }
        Err(error) => {
            #[cfg(windows)]
            if let Some(conn) = conversion_db_for_rollback.take() {
                let database_path = root_dir.join(DATABASE);
                let close_error = if std::env::var("LIBRA_TEST").as_deref() == Ok("1")
                    && std::env::var("LIBRA_TEST_INIT_CONVERT_CLOSE_FAIL").as_deref() == Ok("1")
                {
                    Some("injected database close failure".to_string())
                } else {
                    match tokio::time::timeout(std::time::Duration::from_secs(10), async {
                        db::close_cached_db_conn_instance_for_path(&database_path).await?;
                        conn.close().await.map_err(|error| {
                            io::Error::other(format!(
                                "cannot close new database connection '{}': {error}",
                                database_path.display()
                            ))
                        })
                    })
                    .await
                    {
                        Ok(Ok(())) => None,
                        Ok(Err(close_error)) => Some(close_error.to_string()),
                        Err(_) => Some("timed out after 10 seconds".to_string()),
                    }
                };
                if let Some(detail) = close_error {
                    return Err(InitError::ConversionRetryRefused {
                        code: StableErrorCode::RepoStateInvalid,
                        message: format!(
                            "{error}; cannot close repository database '{}' before conversion rollback: {detail}",
                            database_path.display()
                        ),
                        hint: init_conversion::manual_recovery_hint(&root_dir),
                    });
                }
            }
            if let Some(mut guard) = conversion_guard.take() {
                let failures = guard.rollback();
                if !failures.is_empty() {
                    let detail = failures
                        .iter()
                        .map(|(path, why)| format!("{}: {why}", path.display()))
                        .collect::<Vec<_>>()
                        .join("; ");
                    return Err(InitError::ConversionRetryRefused {
                        code: StableErrorCode::RepoStateInvalid,
                        message: format!(
                            "{error}; rolling back the partially converted repository failed ({detail})"
                        ),
                        hint: init_conversion::manual_recovery_hint(&root_dir),
                    });
                }
            }
            Err(error)
        }
    }
}

/// Test-only conversion gate (ADR-IG-05). Inert unless `LIBRA_TEST=1`.
fn conversion_gate(stage: init_conversion::ConversionStage) -> Result<(), InitError> {
    match init_conversion::conversion_failpoint(stage) {
        init_conversion::ConversionFailpoint::Proceed => Ok(()),
        init_conversion::ConversionFailpoint::Fail => Err(InitError::Io(std::io::Error::other(
            format!("injected Git conversion failure at '{}'", stage.key()),
        ))),
        // Simulates a hard kill: the marker must let the next run recover.
        init_conversion::ConversionFailpoint::Abort => std::process::abort(),
        init_conversion::ConversionFailpoint::Pause => {
            init_conversion::wait_for_resume_file();
            Ok(())
        }
    }
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
    target_dir: &Path,
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
        let requested_ref_format = resolve_ref_format(args.ref_format.as_deref())?;
        validate_branch_name(requested, &requested_ref_format)?;
    }
    let requested_object_format = args
        .object_format
        .as_deref()
        .map(|format| resolve_object_format(Some(format)))
        .transpose()?;

    // ADR-B3-01: compare stored objectformat via a read-only, no-migration
    // connection BEFORE layout/DB upgrade side effects.
    let database_path = root_dir.join(DATABASE);
    let inspect_conn = crate::internal::db::open_database_without_migrations(&database_path)
        .await
        .map_err(InitError::Io)?;
    let stored_object_format = read_config_string(&inspect_conn, "core.objectformat")
        .await?
        .unwrap_or_else(|| "sha1".to_string());
    // Drop the inspect connection before opening the migrating one.
    let _ = inspect_conn.close().await;

    if crate::internal::object_format::parse_config_value(&stored_object_format).is_err() {
        return Err(InitError::UnknownObjectFormat {
            message: format!("repository has unknown core.objectformat '{stored_object_format}'"),
        });
    }
    if let Some(requested) = &requested_object_format
        && requested != &stored_object_format
    {
        return Err(InitError::ReinitObjectFormatMismatch {
            stored: stored_object_format.clone(),
            requested: requested.clone(),
        });
    }

    progress.emit("Reinitializing existing repository ...");
    fs::create_dir_all(root_dir)?;
    prepare_repository_layout(
        root_dir,
        target_dir,
        template_dir,
        args.template.as_deref() != Some(""),
    )?;
    set_dir_hidden(root_dir)?;
    if let Some(shared_mode) = shared_mode {
        // `apply_shared` skips the vault database/sidecars, so private signing
        // material keeps its owner-only mode through the chmod sweep.
        apply_shared(root_dir, shared_mode)?;
    }

    // Connect to the EXISTING database (schema auto-upgrades on open); never recreate
    // it — `create_database_connection` would reject an existing file.
    let conn = get_db_conn_instance_for_path(&database_path)
        .await
        .map_err(InitError::Io)?;
    if let Some(shared_mode) = shared_mode {
        persist_shared_repository(&conn, shared_mode).await?;
    }

    let object_format = stored_object_format;
    let ref_format = read_config_string(&conn, "core.initrefformat")
        .await?
        .unwrap_or_else(|| RefFormat::Strict.as_str().to_string());
    let repo_id = read_config_string(&conn, "libra.repoid")
        .await?
        .unwrap_or_default();
    let bare = read_config_string(&conn, "core.bare")
        .await?
        .and_then(|value| crate::internal::config::parse_git_bool(&value))
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

    set_hash_kind(
        crate::internal::object_format::parse_config_value(&object_format).map_err(|_| {
            InitError::UnknownObjectFormat {
                message: format!("repository has unknown core.objectformat '{object_format}'"),
            }
        })?,
    );

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

    let path = root_dir
        .canonicalize()
        .unwrap_or_else(|_| root_dir.to_path_buf())
        .to_string_lossy()
        .to_string();
    Ok(InitOutput {
        path,
        storage_path: root_dir.to_path_buf(),
        bare,
        initial_branch,
        ref_format,
        object_format,
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
async fn read_config_string<C: sea_orm::ConnectionTrait>(
    conn: &C,
    key: &str,
) -> Result<Option<String>, InitError> {
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

struct BareTargetSnapshot {
    missing_layout_names: std::collections::HashSet<std::ffi::OsString>,
}

fn new_bare_layout_entries(
    root: &Path,
    snapshot: &BareTargetSnapshot,
) -> io::Result<Vec<(PathBuf, bool)>> {
    let mut created = Vec::new();
    for name in &snapshot.missing_layout_names {
        if name == std::ffi::OsStr::new(init_conversion::CONVERSION_MARKER) {
            continue;
        }
        let path = root.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => created.push((path, metadata.is_dir())),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(created)
}

fn rollback_bare_layout(root: &Path, snapshot: &BareTargetSnapshot) -> io::Result<()> {
    for (path, directory) in new_bare_layout_entries(root, snapshot)? {
        let result = if directory {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        if let Err(error) = result
            && error.kind() != ErrorKind::NotFound
        {
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "cannot remove new layout entry '{}': {error}",
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}

fn resolve_init_target(base: &Path, raw: &str) -> Result<PathBuf, InitError> {
    let path = resolve_cli_path(base, raw);
    resolve_template_target_path(&path).map_err(|source| {
        InitError::Io(io::Error::new(
            source.kind(),
            format!(
                "failed to resolve target repository '{}': {source}; choose an accessible target path",
                path.display()
            ),
        ))
    })
}

/// Resolve an init target that may not exist yet, preserving filesystem order
/// across symlinks and `..`. Once a component is missing, a following `..`
/// cancels that missing component; when the pending suffix is empty again,
/// existing components are resolved by the kernel. A dangling symlink or a
/// non-NotFound filesystem error is refused rather than guessed.
fn resolve_template_target_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };
    // Win32 normalizes parent components before resolving reparse points.
    // Keep the safety check and the path ultimately used by init aligned.
    #[cfg(windows)]
    let absolute = crate::utils::util::normalize_abs_path(&absolute);
    let mut resolved = PathBuf::new();
    let mut missing = Vec::<std::ffi::OsString>::new();
    let mut components = absolute.components().peekable();
    while let Some(component) = components.next() {
        match component {
            std::path::Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
            std::path::Component::RootDir => resolved.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if missing.pop().is_none() {
                    resolved.pop();
                }
            }
            std::path::Component::Normal(name) => {
                if !missing.is_empty() {
                    missing.push(name.to_os_string());
                    continue;
                }
                let candidate = resolved.join(name);
                match fs::canonicalize(&candidate) {
                    Ok(real) => {
                        if components.peek().is_some() && !fs::metadata(&real)?.is_dir() {
                            return Err(io::Error::new(
                                ErrorKind::NotADirectory,
                                format!(
                                    "target path component '{}' is not a directory",
                                    candidate.display()
                                ),
                            ));
                        }
                        resolved = real;
                    }
                    Err(error) if error.kind() == ErrorKind::NotFound => {
                        match fs::symlink_metadata(&candidate) {
                            Err(missing_error) if missing_error.kind() == ErrorKind::NotFound => {
                                missing.push(name.to_os_string());
                            }
                            Err(other) => return Err(other),
                            Ok(_) => {
                                return Err(io::Error::new(
                                    ErrorKind::InvalidInput,
                                    format!(
                                        "cannot resolve target through dangling path '{}'",
                                        candidate.display()
                                    ),
                                ));
                            }
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }
    for name in missing {
        resolved.push(name);
    }
    Ok(resolved)
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

/// ADR-IN-03 item 3: `--template` > `GIT_TEMPLATE_DIR` > `init.templateDir`
/// (strict local→global→system cascade) > built-in defaults. `~` expands to HOME.
/// Only reached when no `--template` was given; an empty `--template=` never
/// falls through to these sources.
async fn resolve_template_from_sources() -> Result<Option<PathBuf>, InitError> {
    if let Some(env) = std::env::var_os("GIT_TEMPLATE_DIR").filter(|value| !value.is_empty()) {
        return resolve_template_path(&cur_dir(), &env.to_string_lossy()).map(Some);
    }
    match read_cascaded_config_value_strict(LocalIdentityTarget::CurrentRepo, "init.templateDir")
        .await
    {
        Ok(Some(raw)) => {
            let expanded = expand_home(&raw);
            resolve_template_path(&cur_dir(), &expanded).map(Some)
        }
        Ok(None) => Ok(None),
        Err(error) => Err(InitError::ConfigRead {
            key: "init.templateDir".to_string(),
            detail: format!("{error:#}"),
        }),
    }
}

fn expand_home(raw: &str) -> String {
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest).to_string_lossy().into_owned();
    }
    raw.to_string()
}

/// ADR-IN-02 item 1: ref-format values are a CLI usage choice; Git's storage
/// backends `files`/`reftable` are documented intentional differences (D25).
fn resolve_ref_format(raw: Option<&str>) -> Result<RefFormat, InitError> {
    match raw {
        None | Some("strict") => Ok(RefFormat::Strict),
        Some("filesystem") => Ok(RefFormat::Filesystem),
        Some("files") | Some("reftable") => Err(invalid_argument(
            format!(
                "--ref-format={} is not implemented; Libra refs live in SQLite and \
                 --ref-format only selects branch-name validation",
                raw.unwrap_or_default()
            ),
            Some("use --ref-format=strict (default) or --ref-format=filesystem".to_string()),
        )),
        Some(other) => Err(invalid_argument(
            format!("unknown ref format '{other}'"),
            Some("supported values: strict, filesystem".to_string()),
        )),
    }
}

/// ADR-IN-02 item 2: a storage path that exists but is not a directory is a
/// repository-state conflict (LBR-REPO-002/128), detected before any write.
fn ensure_storage_paths_are_directories(target_dir: &Path, bare: bool) -> Result<(), InitError> {
    // The bare database file is a FILE by design, so only the directory-like
    // layout paths are type-checked.
    let paths = if bare {
        vec![
            target_dir.join("objects"),
            target_dir.join("info"),
            target_dir.join("hooks"),
        ]
    } else {
        vec![target_dir.join(ROOT_DIR)]
    };
    for path in paths {
        if let Ok(metadata) = fs::symlink_metadata(&path)
            && !metadata.is_dir()
        {
            return Err(InitError::StoragePathNotDirectory { path });
        }
    }
    Ok(())
}

/// ADR-IN-06 source cascade: `--object-format`, then `GIT_DEFAULT_HASH`, then
/// `init.defaultObjectFormat`, then `sha1`. Invalid config falls back with a
/// warning; invalid env is a usage error. Not used on reinit or
/// `--from-git-repository`.
async fn resolve_object_format_from_sources(cli: Option<&str>) -> Result<String, InitError> {
    if let Some(raw) = cli {
        return resolve_object_format(Some(raw));
    }
    if let Some(env) = std::env::var_os("GIT_DEFAULT_HASH").filter(|value| !value.is_empty()) {
        return resolve_object_format(Some(&env.to_string_lossy()));
    }
    match read_cascaded_config_value_strict(
        LocalIdentityTarget::CurrentRepo,
        "init.defaultObjectFormat",
    )
    .await
    {
        Ok(Some(raw)) => {
            if crate::internal::object_format::parse_config_value(&raw.to_ascii_lowercase())
                .is_err()
            {
                crate::utils::error::emit_warning(format!(
                    "unknown hash algorithm '{raw}'; falling back to sha1"
                ));
                return Ok("sha1".to_string());
            }
            resolve_object_format(Some(&raw))
        }
        Ok(None) => Ok("sha1".to_string()),
        Err(error) => Err(InitError::ConfigRead {
            key: "init.defaultObjectFormat".to_string(),
            detail: format!("{error:#}"),
        }),
    }
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
    crate::internal::object_format::parse_config_value(&object_format)
        .map(|kind| crate::internal::object_format::as_str(kind).to_string())
        .map_err(|_| {
            invalid_argument(
                format!("unsupported object format '{object_format}'"),
                suggest_object_format(&object_format)
                    .map(|suggestion| format!("did you mean '{suggestion}'?")),
            )
        })
}

fn suggest_object_format(value: &str) -> Option<&'static str> {
    match value {
        "sha265" | "sha-256" => Some("sha256"),
        "blake" | "blake2" | "blak3" => Some("blake3"),
        _ => None,
    }
}

/// ADR-B3-01 Convert gate: only SHA-1 Git → SHA-1 Libra is allowed, and the
/// check must run before any target layout/DB write.
fn refuse_convert_object_format_combination(
    source: &Path,
    target_format: &str,
) -> Result<(), InitError> {
    let fresh_init_hint = Some(
        "SHA-256/BLAKE3 Libra repositories can only be created with a fresh \
         `libra init --object-format <format>` (Convert from Git is SHA-1 only)"
            .to_string(),
    );
    if target_format != "sha1" {
        return Err(InitError::ConvertObjectFormatForbidden {
            message: format!(
                "cannot convert a Git repository into a Libra repository with \
                 --object-format {target_format}"
            ),
            hint: fresh_init_hint.clone(),
        });
    }

    // Resolve worktree → `.git` (or bare root) before reading `config`.
    let git_dir = convert::resolve_git_source_dir(source)?;
    let source_kind = crate::internal::protocol::local_client::read_git_source_objectformat(
        &git_dir,
    )
    .map_err(|error| match error {
        crate::internal::protocol::local_client::GitSourceObjectFormatError::Unreadable(
            message,
        ) => InitError::Io(io::Error::other(message)),
        crate::internal::protocol::local_client::GitSourceObjectFormatError::UnknownValue(
            value,
        ) => InitError::UnknownObjectFormat {
            message: format!(
                "source Git repository has unsupported extensions.objectformat '{value}'"
            ),
        },
        crate::internal::protocol::local_client::GitSourceObjectFormatError::ExtensionWithoutV1 => {
            InitError::UnknownObjectFormat {
                message: "source Git repository sets extensions.objectformat without \
                              core.repositoryformatversion=1"
                    .to_string(),
            }
        }
        crate::internal::protocol::local_client::GitSourceObjectFormatError::Corrupt(message) => {
            InitError::UnknownObjectFormat { message }
        }
    })?;

    if matches!(source_kind, git_internal::hash::HashKind::Sha256) {
        return Err(InitError::ConvertObjectFormatForbidden {
            message: "cannot convert a SHA-256 Git repository into Libra".to_string(),
            hint: fresh_init_hint,
        });
    }
    Ok(())
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

fn prepare_repository_layout(
    root_dir: &Path,
    target_dir: &Path,
    template_dir: Option<&Path>,
    builtin_defaults: bool,
) -> io::Result<()> {
    if let Some(template_dir) = template_dir {
        // The target now exists. Re-resolve it after creation so the source
        // walker and the skip use the same canonical spelling, even when the
        // user named the target through a symlink or `..`.
        let target = fs::canonicalize(target_dir)?;
        let root = fs::canonicalize(root_dir)?;
        if template_dir == target || template_dir == root {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "template directory '{}' is the target repository itself",
                    template_dir.display()
                ),
            ));
        }
        if template_dir.starts_with(&root) {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "template directory '{}' is inside the target storage directory '{}'",
                    template_dir.display(),
                    root.display()
                ),
            ));
        }
        let mut skips = Vec::new();
        if target.starts_with(template_dir) {
            skips.push(target);
        }
        if root.starts_with(template_dir) && !skips.contains(&root) {
            skips.push(root);
        }
        copy_template(template_dir, root_dir, &skips)?;
    } else if builtin_defaults {
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

fn copy_template(src: &Path, dst: &Path, skips: &[PathBuf]) -> io::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let source_path = entry.path();
        let dest_path = dst.join(entry.file_name());

        // ADR-IN-03 item 2: when the template is an ANCESTOR of the storage
        // root, the storage subtree would recurse into itself. Skip it.
        if skips
            .iter()
            .any(|skip| source_path == *skip || dest_path == *skip)
        {
            continue;
        }

        if file_type.is_dir() {
            // Never recurse through a symlinked destination directory: it would follow
            // the link and write template files outside the repository.
            refuse_symlinked_layout_path(&dest_path)?;
            fs::create_dir_all(&dest_path)?;
            copy_template(&source_path, &dest_path, skips)?;
        } else if fs::symlink_metadata(&dest_path).is_err() {
            // `symlink_metadata` does not follow links, so a symlinked destination is
            // left untouched rather than written through; only truly-absent files are
            // copied (Git-style top-up that preserves customized files).
            fs::copy(&source_path, &dest_path)?;
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
fn set_shared_path_permissions(root_dir: &Path, path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let is_transient_file = path.parent().is_some_and(|parent| {
        parent == root_dir.join("object-index-repair")
            || parent == root_dir.join("object-index-repair-tmp")
            || (parent == root_dir
                && matches!(
                    path.file_name().and_then(|name| name.to_str()),
                    Some("libra.db-journal" | "libra.db-wal" | "libra.db-shm")
                ))
    });
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if is_transient_file && error.kind() == ErrorKind::NotFound => {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let mut perms = metadata.permissions();
    perms.set_mode(mode);
    match fs::set_permissions(path, perms) {
        Ok(()) => Ok(()),
        // Repair files and SQLite sidecars are retired concurrently after their
        // durable work commits. Their disappearance means there is no remaining
        // path whose permissions need migration.
        Err(error) if is_transient_file && error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(not(target_os = "windows"))]
fn apply_shared(root_dir: &Path, shared_mode: &SharedMode) -> io::Result<()> {
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
            set_shared_path_permissions(dir, path, mode)?;
        }
        Ok(())
    }

    if let Some(bits) = shared_mode.permission_bits() {
        set_recursive(root_dir, bits)?;
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn apply_shared_bare_created(
    root_dir: &Path,
    snapshot: &BareTargetSnapshot,
    shared_mode: &SharedMode,
) -> io::Result<()> {
    let Some(bits) = shared_mode.permission_bits() else {
        return Ok(());
    };
    set_shared_path_permissions(root_dir, root_dir, bits)?;
    for (path, _) in new_bare_layout_entries(root_dir, snapshot)? {
        apply_shared(&path, shared_mode)?;
    }
    for name in [
        DATABASE.to_string(),
        format!("{DATABASE}-wal"),
        format!("{DATABASE}-shm"),
        format!("{DATABASE}-journal"),
    ] {
        set_shared_path_permissions(root_dir, &root_dir.join(name), bits)?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn apply_shared(_root_dir: &Path, _shared_mode: &SharedMode) -> io::Result<()> {
    Ok(())
}

#[cfg(target_os = "windows")]
fn apply_shared_bare_created(
    _root_dir: &Path,
    _snapshot: &BareTargetSnapshot,
    _shared_mode: &SharedMode,
) -> io::Result<()> {
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
    let txn = crate::internal::db::begin_write_transaction(conn).await?;

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
    #[cfg(unix)]
    use std::fs;
    use std::{
        io::{Read, Write},
        path::PathBuf,
    };

    use gag::BufferRedirect;
    use serial_test::serial;
    use tempfile::tempdir;

    #[cfg(unix)]
    use super::set_shared_path_permissions;
    use super::{DEFAULT_BRANCH, InitArgs, InitError, run_init};
    use crate::utils::test::{self, ChangeDirGuard};

    #[cfg(unix)]
    #[test]
    fn shared_permissions_tolerate_retired_index_repair_files() {
        let root = tempdir().expect("failed to create storage root");
        for directory in ["object-index-repair", "object-index-repair-tmp"] {
            let parent = root.path().join(directory);
            fs::create_dir(&parent).expect("failed to create repair directory");
            let retired = parent.join("retired.blob.json");

            set_shared_path_permissions(root.path(), &retired, 0o2777)
                .expect("a concurrently retired repair file should not fail shared init");
        }
    }

    #[cfg(unix)]
    #[test]
    fn shared_permissions_tolerate_retired_repository_database_sidecars() {
        let root = tempdir().expect("failed to create storage root");
        for file_name in ["libra.db-journal", "libra.db-wal", "libra.db-shm"] {
            let retired = root.path().join(file_name);

            set_shared_path_permissions(root.path(), &retired, 0o2777)
                .expect("a concurrently retired database sidecar should not fail shared init");
        }
    }

    #[cfg(unix)]
    #[test]
    fn shared_permissions_reject_a_missing_ordinary_path() {
        let root = tempdir().expect("failed to create storage root");
        let error = set_shared_path_permissions(root.path(), &root.path().join("missing"), 0o2777)
            .expect_err("ordinary missing paths must remain errors");

        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn shared_permissions_update_a_live_index_repair_file() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempdir().expect("failed to create storage root");
        let parent = root.path().join("object-index-repair");
        fs::create_dir(&parent).expect("failed to create repair directory");
        let marker = parent.join("live.blob.json");
        fs::write(&marker, b"marker").expect("failed to write repair marker");
        fs::set_permissions(&marker, fs::Permissions::from_mode(0o600))
            .expect("failed to seed marker mode");

        set_shared_path_permissions(root.path(), &marker, 0o2777)
            .expect("live repair marker should receive shared permissions");

        let mode = fs::metadata(&marker)
            .expect("failed to stat repair marker")
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(mode, 0o2777);
    }

    #[cfg(unix)]
    #[test]
    fn shared_permissions_update_a_live_repository_database_sidecar() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempdir().expect("failed to create storage root");
        let sidecar = root.path().join("libra.db-journal");
        fs::write(&sidecar, b"journal").expect("failed to write database sidecar");
        fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o600))
            .expect("failed to seed sidecar mode");

        set_shared_path_permissions(root.path(), &sidecar, 0o2777)
            .expect("live database sidecar should receive shared permissions");

        let mode = fs::metadata(&sidecar)
            .expect("failed to stat database sidecar")
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(mode, 0o2777);
    }

    #[test]
    fn init_error_display_pins_owned_variants() {
        assert_eq!(
            InitError::ConfigRead {
                key: "init.defaultBranch".to_string(),
                detail: "database is unreadable".to_string(),
            }
            .to_string(),
            "failed to read config 'init.defaultBranch': database is unreadable",
        );
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
    #[serial(cwd, env)]
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
            separate_git_dir: None,
            separate_libra_dir: None,
            recurse_submodules: false,
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
