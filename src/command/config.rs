//! Config command for reading and writing settings across scopes.
//!
//! Supports subcommand style (`libra config set/get/list/unset/import/path`)
//! and Git-compatible flag style (`--get`, `--list`, etc.).

use std::{io::IsTerminal, path::PathBuf, process::Command};

use clap::{Parser, Subcommand};
use once_cell::sync::Lazy;
use sea_orm::{DatabaseConnection, TransactionTrait};
use serde::Serialize;
use tokio::sync::Mutex;

use crate::{
    internal::{
        config::{ConfigKv, ConfigKvEntry, is_sensitive_key, is_vault_internal_key},
        db::{create_database, establish_connection, get_db_conn_instance},
        upgrade::settings::{
            UPGRADE_MODE_KEY, UpgradeMode, UpgradeSettingsError, read_mode as read_upgrade_mode,
            settings_path as upgrade_settings_path, write_mode as write_upgrade_mode,
        },
        vault::{
            decrypt_token, encrypt_token, generate_pgp_key, generate_ssh_key_pair,
            lazy_init_vault_for_scope, load_unseal_key_for_scope,
        },
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode, emit_warning},
        output::{OutputConfig, emit_json_data},
        pager::LIBRA_TEST_ENV,
        text::levenshtein,
        util::{DATABASE, try_get_storage_path},
    },
};

/// Cached database connection for Global scope, paired with the resolved DB path.
static GLOBAL_CONFIG_CONN: Lazy<Mutex<Option<(PathBuf, DatabaseConnection)>>> =
    Lazy::new(|| Mutex::new(None));

/// Cached database connection for System scope, paired with the resolved DB path.
static SYSTEM_CONFIG_CONN: Lazy<Mutex<Option<(PathBuf, DatabaseConnection)>>> =
    Lazy::new(|| Mutex::new(None));

const EXAMPLES: &str = r#"EXAMPLES:
    libra config set user.name "John Doe"              Set local config value
    libra config get user.name                         Get value (cascade lookup)
    libra config --type int core.editorTimeout 30       Validate/canonicalize a typed value on set
    libra config list                                  List all local entries
    libra config list --show-origin                    List with scope labels
    libra config set --global user.email "j@x.com"     Set global config
    libra config set --global upgrade.mode auto        Enable auto-upgrade (reserved namespace)
    libra config set --system core.editor vim           Set system-wide config (needs privileges)
    libra config unset user.signingkey                 Remove a key
    libra config import --global                       Import from Git global config
    libra config set vault.env.GEMINI_API_KEY          Store API key (interactive)
    echo "$SECRET" | libra config set --stdin vault.env.KEY  Set from stdin (CI/CD)
    libra config set --encrypt custom.key "value"      Force-encrypt a value
    libra config list --vault                          List vault env entries
    libra config generate-ssh-key --remote origin      Generate SSH key for remote
    libra config generate-gpg-key                      Generate GPG signing key
    libra config list --name-only                      List all key names
    libra config path                                  Show config DB path"#;

/// Configuration scope that determines where values are stored and retrieved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigScope {
    /// Repository-specific (`.libra/libra.db`). Default for writes.
    Local,
    /// User-level (`~/.libra/config.db`).
    Global,
    /// System-wide (`/etc/libra/config.db`, overridable via
    /// `LIBRA_CONFIG_SYSTEM_DB`). Lowest precedence; writing it usually needs
    /// elevated privileges, like Git's `/etc/gitconfig`.
    System,
}

impl ConfigScope {
    /// Cascade order for reads (highest to lowest precedence): local overrides
    /// global, which overrides system — matching Git.
    pub const CASCADE_ORDER: [ConfigScope; 3] =
        [ConfigScope::Local, ConfigScope::Global, ConfigScope::System];

    /// Get the config database path for this scope.
    pub fn get_config_path(&self) -> Option<PathBuf> {
        match self {
            ConfigScope::Local => None,
            ConfigScope::Global => {
                if let Some(p) = std::env::var_os("LIBRA_CONFIG_GLOBAL_DB") {
                    return Some(PathBuf::from(p));
                }
                dirs::home_dir().map(|home| home.join(".libra").join("config.db"))
            }
            ConfigScope::System => {
                if let Some(p) = std::env::var_os("LIBRA_CONFIG_SYSTEM_DB") {
                    return Some(PathBuf::from(p));
                }
                Some(PathBuf::from("/etc/libra/config.db"))
            }
        }
    }

    pub async fn ensure_config_exists(&self) -> Result<(), String> {
        match self {
            ConfigScope::Local => Ok(()),
            ConfigScope::Global | ConfigScope::System => {
                let label = scope_name(*self);
                if let Some(config_path) = self.get_config_path() {
                    if let Some(parent_dir) = config_path.parent()
                        && !parent_dir.exists()
                    {
                        std::fs::create_dir_all(parent_dir).map_err(|e| {
                            format!(
                                "Failed to create {label} config directory '{}': {e}{}",
                                parent_dir.display(),
                                if matches!(self, ConfigScope::System) {
                                    " (writing system config usually requires elevated privileges)"
                                } else {
                                    ""
                                }
                            )
                        })?;
                    }
                    if !config_path.exists() {
                        let config_path_str = config_path.to_string_lossy();
                        create_database(&config_path_str).await.map_err(|e| {
                            format!("Failed to create {label} config database: {e}")
                        })?;
                    }
                    Ok(())
                } else {
                    Err(format!(
                        "Could not determine {label} config path: home directory not available"
                    ))
                }
            }
        }
    }
}

/// Scoped config access layer — resolves the correct database for each scope.
pub struct ScopedConfig;

impl ScopedConfig {
    /// Get a database connection for the specified scope.
    pub async fn get_connection(scope: ConfigScope) -> Result<DatabaseConnection, String> {
        match scope {
            ConfigScope::Local => {
                let storage = try_get_storage_path(None).map_err(|_| {
                    "fatal: not a libra repository (or any of the parent directories): .libra"
                        .to_string()
                })?;
                let db_path = storage.join(DATABASE);
                if !db_path.exists() {
                    return Err(format!(
                        "fatal: libra database not found at '{}'",
                        db_path.display()
                    ));
                }
                Ok(get_db_conn_instance().await.clone())
            }
            ConfigScope::Global => {
                Self::get_or_create_cached_connection(&GLOBAL_CONFIG_CONN, scope, "global").await
            }
            ConfigScope::System => {
                Self::get_or_create_cached_connection(&SYSTEM_CONFIG_CONN, scope, "system").await
            }
        }
    }

    async fn get_or_create_cached_connection(
        cache: &Lazy<Mutex<Option<(PathBuf, DatabaseConnection)>>>,
        scope: ConfigScope,
        scope_name: &str,
    ) -> Result<DatabaseConnection, String> {
        let Some(config_path) = scope.get_config_path() else {
            return Err(format!(
                "Could not determine config path for {scope_name} scope"
            ));
        };
        let mut guard = cache.lock().await;
        if let Some((cached_path, cached_conn)) = guard.as_ref() {
            if cached_path == &config_path {
                return Ok(cached_conn.clone());
            }
            *guard = None;
        }
        scope.ensure_config_exists().await?;
        let config_path_str = config_path.to_string_lossy();
        let conn = establish_connection(&config_path_str)
            .await
            .map_err(|e| format!("Failed to connect to {scope_name} config database: {e}"))?;
        *guard = Some((config_path, conn.clone()));
        Ok(conn)
    }

    // ── ConfigKv wrappers with scope ─────────────────────────────────

    pub async fn get(scope: ConfigScope, key: &str) -> Result<Option<ConfigKvEntry>, String> {
        let conn = Self::get_connection(scope).await?;
        ConfigKv::get_with_conn(&conn, key)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn get_all(scope: ConfigScope, key: &str) -> Result<Vec<ConfigKvEntry>, String> {
        let conn = Self::get_connection(scope).await?;
        ConfigKv::get_all_with_conn(&conn, key)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn set(
        scope: ConfigScope,
        key: &str,
        value: &str,
        encrypted: bool,
    ) -> Result<(), String> {
        let conn = Self::get_connection(scope).await?;
        ConfigKv::set_with_conn(&conn, key, value, encrypted)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn add(
        scope: ConfigScope,
        key: &str,
        value: &str,
        encrypted: bool,
    ) -> Result<(), String> {
        let conn = Self::get_connection(scope).await?;
        ConfigKv::add_with_conn(&conn, key, value, encrypted)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn unset(scope: ConfigScope, key: &str) -> Result<usize, String> {
        let conn = Self::get_connection(scope).await?;
        ConfigKv::unset_with_conn(&conn, key)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn unset_all(scope: ConfigScope, key: &str) -> Result<usize, String> {
        let conn = Self::get_connection(scope).await?;
        ConfigKv::unset_all_with_conn(&conn, key)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn list_all(scope: ConfigScope) -> Result<Vec<ConfigKvEntry>, String> {
        let conn = Self::get_connection(scope).await?;
        ConfigKv::list_all_with_conn(&conn)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn get_by_prefix(
        scope: ConfigScope,
        prefix: &str,
    ) -> Result<Vec<ConfigKvEntry>, String> {
        let conn = Self::get_connection(scope).await?;
        ConfigKv::get_by_prefix_with_conn(&conn, prefix)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn get_regexp(
        scope: ConfigScope,
        pattern: &str,
    ) -> Result<Vec<ConfigKvEntry>, String> {
        let conn = Self::get_connection(scope).await?;
        ConfigKv::get_regexp_with_conn(&conn, pattern)
            .await
            .map_err(|e| e.to_string())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CLI argument definitions
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    about = "Manage repository configurations",
    after_help = EXAMPLES
)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: Option<ConfigCommand>,

    // ── Git-compat flags (hidden, translated to subcommands) ─────────
    /// Get a configuration value
    #[clap(long, hide = true)]
    pub get: bool,
    /// Get all values for a key
    #[clap(long("get-all"), hide = true)]
    pub get_all: bool,
    /// Remove a configuration entry
    #[clap(long, hide = true)]
    pub unset: bool,
    /// Remove all entries for a key
    #[clap(long("unset-all"), hide = true)]
    pub unset_all: bool,
    /// List all entries
    #[clap(long, short, hide = true)]
    pub list: bool,
    /// Add a value (allows duplicates)
    #[clap(long, hide = true)]
    pub add: bool,
    /// Import from Git config
    #[clap(long, hide = true)]
    pub import: bool,
    /// Get entries matching a regex
    #[clap(long("get-regexp"), hide = true)]
    pub get_regexp: bool,
    /// Show which scope each value comes from
    #[clap(long("show-origin"), hide = true)]
    pub show_origin: bool,
    /// Remove an entire section (`<name>`) and all of its keys
    #[clap(long("remove-section"), hide = true)]
    pub remove_section: bool,
    /// Rename a section: `--rename-section <old> <new>`
    #[clap(long("rename-section"), hide = true)]
    pub rename_section: bool,
    /// NUL-terminate output records (`git config -z`): values for get/get-all,
    /// and `key\nvalue\0` for `--get-regexp` / `--list`.
    #[clap(short = 'z', long = "null", global = true)]
    pub null: bool,
    /// Canonicalize the value to a type when reading (`git config --type=<t>`:
    /// `bool`, `int`, or `path`). Mutually exclusive with the shortcut flags.
    #[clap(
        long = "type",
        value_name = "TYPE",
        hide = true,
        group = "config_type_sel"
    )]
    pub value_type: Option<String>,
    /// Shortcut for `--type=bool`.
    #[clap(long = "bool", hide = true, group = "config_type_sel")]
    pub type_bool: bool,
    /// Shortcut for `--type=int`.
    #[clap(long = "int", hide = true, group = "config_type_sel")]
    pub type_int: bool,
    /// Shortcut for `--type=path`.
    #[clap(long = "path", hide = true, group = "config_type_sel")]
    pub type_path: bool,

    // ── Scope flags ──────────────────────────────────────────────────
    /// Use repository config (default)
    #[clap(long, global = true, group("scope"))]
    pub local: bool,
    /// Use global user config
    #[clap(long, global = true, group("scope"))]
    pub global: bool,
    /// Use system-wide config (`/etc/libra/config.db`, overridable via
    /// `LIBRA_CONFIG_SYSTEM_DB`). Lowest cascade precedence; writing it usually
    /// requires elevated privileges. Vault-encrypted secrets are not supported
    /// in this scope.
    #[clap(long, global = true, group("scope"))]
    pub system: bool,

    // ── Positional args (Git-compat mode) ────────────────────────────
    /// Configuration key
    #[clap(value_name = "key")]
    pub key: Option<String>,
    /// Value or value pattern
    #[clap(value_name = "value")]
    pub valuepattern: Option<String>,
    /// Default value when key not found
    #[clap(long, short = 'd')]
    pub default: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Set a configuration value
    Set {
        /// Configuration key (dotted format, e.g. user.name)
        key: String,
        /// Value to set (interactive input for sensitive keys if omitted)
        value: Option<String>,
        /// Add as additional value (allows duplicates)
        #[clap(long)]
        add: bool,
        /// Force vault encryption
        #[clap(long)]
        encrypt: bool,
        /// Force plaintext storage (skip auto-encryption)
        #[clap(long)]
        plaintext: bool,
        /// Read value from stdin
        #[clap(long)]
        stdin: bool,
    },
    /// Get a configuration value
    Get {
        /// Configuration key (or regex pattern with --regexp)
        key: String,
        /// Get all values for this key
        #[clap(long)]
        all: bool,
        /// Show actual value for encrypted entries
        #[clap(long)]
        reveal: bool,
        /// Treat key as regex pattern
        #[clap(long)]
        regexp: bool,
        /// Default value if key not found
        #[clap(long, short = 'd')]
        default: Option<String>,
    },
    /// List configuration entries
    List {
        /// Show only key names
        #[clap(long("name-only"))]
        name_only: bool,
        /// Show scope origin for each entry
        #[clap(long("show-origin"))]
        show_origin: bool,
        /// Show only vault.env.* entries
        #[clap(long)]
        vault: bool,
        /// Show SSH keys
        #[clap(long("ssh-keys"))]
        ssh_keys: bool,
        /// Show GPG keys
        #[clap(long("gpg-keys"))]
        gpg_keys: bool,
    },
    /// Remove a configuration entry
    Unset {
        /// Configuration key to remove
        key: String,
        /// Remove all values for this key
        #[clap(long)]
        all: bool,
    },
    /// Import configuration from Git
    Import,
    /// Show config database file path
    Path,
    /// Open config in editor (not supported — SQLite storage)
    Edit,
    /// Generate SSH key for a remote
    GenerateSshKey {
        /// Remote name to bind the new SSH key to
        #[clap(long, value_name = "NAME")]
        remote: String,
    },
    /// Generate GPG key for signing
    GenerateGpgKey {
        /// User name for the key (default: from `user.name` config)
        #[clap(long, value_name = "NAME")]
        name: Option<String>,
        /// User email for the key (default: from `user.email` config)
        #[clap(long, value_name = "EMAIL")]
        email: Option<String>,
        /// Key usage: `signing` (default) or `encrypt`
        #[clap(long, value_name = "KIND")]
        usage: Option<String>,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// Serializable output types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct ConfigListEntry {
    key: String,
    value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    encrypted: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
struct ConfigImportSummary {
    scope: &'static str,
    imported: usize,
    skipped_duplicates: usize,
    ignored_invalid: usize,
    /// Reserved `upgrade.*` entries skipped by the namespace router (§A.3).
    ignored_reserved: usize,
    auto_encrypted: usize,
    collapsed_multivalue_warnings: usize,
}

#[derive(Debug, Clone, Serialize)]
struct ConfigSshKeyEntry {
    remote: String,
    #[serde(rename = "type")]
    key_type: String,
    public_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    key_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ConfigGpgKeyEntry {
    usage: String,
    #[serde(rename = "type")]
    key_type: String,
    pubkey_config_key: String,
    signing_enabled: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Entry points
// ─────────────────────────────────────────────────────────────────────────────

/// Execute the `config` command, printing any error to stderr.
pub async fn execute(args: ConfigArgs) {
    if let Err(e) = execute_safe(args, &OutputConfig::default()).await {
        e.print_stderr();
    }
}

/// Safe entry point returning structured [`CliResult`].
pub async fn execute_safe(args: ConfigArgs, output: &OutputConfig) -> CliResult<()> {
    execute_inner(args, output).await
}

// ─────────────────────────────────────────────────────────────────────────────
// Dispatch logic
// ─────────────────────────────────────────────────────────────────────────────

async fn execute_inner(args: ConfigArgs, output: &OutputConfig) -> CliResult<()> {
    let scope = get_scope(&args);
    let use_cascade = !has_explicit_scope(&args);

    // Reserved `upgrade.*` namespace (plan-20260714 §A.3), step 1: preflight
    // the RAW spelling before `resolve_command` — legacy-flag resolution is
    // lossy (it picks one action by priority and silently drops the rest), so
    // a reserved operand combined with conflicting/unsupported spellings must
    // fail closed here, not after the intent has been rewritten.
    upgrade_namespace_raw_preflight(&args)?;

    // Resolve subcommand: either explicit or translated from Git-compat flags
    let cmd = resolve_command(&args)?;

    // Reserved `upgrade.*` namespace, step 2: route every resolved command
    // that targets it to `{LIBRA_HOME}/upgrade/settings.json`, never to the
    // SQLite store.
    if let Some(routed) = route_upgrade_namespace(&args, &cmd, output).await {
        return routed;
    }

    match cmd {
        ResolvedCommand::Set {
            key,
            value,
            add,
            encrypt,
            plaintext,
            stdin,
            value_type,
            explicit_set,
            null,
        } => {
            handle_set(
                &key,
                value.as_deref(),
                add,
                encrypt,
                plaintext,
                stdin,
                value_type,
                scope,
                use_cascade,
                explicit_set,
                null,
                output,
            )
            .await
        }
        ResolvedCommand::Get {
            key,
            all,
            reveal,
            regexp,
            default,
            null,
            value_type,
        } => {
            handle_get(
                &key,
                all,
                reveal,
                regexp,
                default.as_deref(),
                scope,
                use_cascade,
                null,
                value_type,
                output,
            )
            .await
        }
        ResolvedCommand::List {
            name_only,
            show_origin,
            vault,
            ssh_keys,
            gpg_keys,
            null,
        } => {
            handle_list(
                name_only,
                show_origin,
                vault,
                ssh_keys,
                gpg_keys,
                scope,
                use_cascade,
                null,
                output,
            )
            .await
        }
        ResolvedCommand::Unset { key, all } => handle_unset(&key, all, scope, output).await,
        ResolvedCommand::RemoveSection { name } => {
            handle_remove_section(&name, scope, output).await
        }
        ResolvedCommand::RenameSection { old, new } => {
            handle_rename_section(&old, &new, scope, output).await
        }
        ResolvedCommand::Import => handle_import(scope, output).await,
        ResolvedCommand::Path => handle_path(scope, output).await,
        ResolvedCommand::Edit => Err(CliError::from_legacy_string(
            "error: config edit is not supported (SQLite storage does not support text-based editing)\n\nhint: use libra config set/unset/list to manage configuration\nhint: use libra config list --name-only to see all keys",
        )),
        ResolvedCommand::GenerateSshKey { remote } => {
            handle_generate_ssh_key(&remote, scope, output).await
        }
        ResolvedCommand::GenerateGpgKey { name, email, usage } => {
            handle_generate_gpg_key(
                name.as_deref(),
                email.as_deref(),
                usage.as_deref(),
                scope,
                output,
            )
            .await
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Reserved `upgrade.*` namespace router (plan-20260714 §A.3)
// ─────────────────────────────────────────────────────────────────────────────

/// Whether `key` (a config key or a `--remove-section`/`--rename-section`
/// section name) falls inside the reserved `upgrade.*` namespace.
fn is_upgrade_namespace_key(key: &str) -> bool {
    let key = key.trim();
    key.eq_ignore_ascii_case("upgrade")
        || key
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("upgrade."))
}

/// Usage error for config spellings outside the supported single-value
/// `--global` contract of the reserved namespace (exit 129, `LBR-CLI-002`;
/// `LBR-UPGRADE-001` is reserved for a damaged settings file).
fn upgrade_namespace_usage_error(operation: &str) -> CliError {
    CliError::command_usage(format!(
        "{operation} is not supported for the reserved upgrade.* namespace"
    ))
    .with_stable_code(StableErrorCode::CliInvalidArguments)
    .with_hint(
        "supported operations: libra config set/get/unset --global upgrade.mode <auto|manual|off>",
    )
}

/// Map a settings-store failure onto the CLI error surface. Every read-side
/// failure (unreadable, corrupt, unresolvable home) is `LBR-UPGRADE-001` per
/// docs/error-codes.md; only write failures use the generic IO write code.
fn upgrade_settings_cli_error(err: UpgradeSettingsError) -> CliError {
    let code = match &err {
        UpgradeSettingsError::WriteFailed { .. } => StableErrorCode::IoWriteFailed,
        UpgradeSettingsError::Home(_)
        | UpgradeSettingsError::Unreadable { .. }
        | UpgradeSettingsError::Invalid { .. } => StableErrorCode::UpgradeSettingsInvalid,
    };
    CliError::fatal(err.to_string()).with_stable_code(code)
}

/// Whether a `--get-regexp` pattern can reach the reserved key: the same
/// `regex::Regex::is_match` semantics as [`ConfigKv::get_regexp_with_conn`].
/// Such patterns fail closed (§A.3); non-matching patterns proceed, with any
/// stale SQLite `upgrade.*` rows suppressed from the result as defense in
/// depth. Invalid patterns fall through to the normal invalid-regex error.
fn regexp_reaches_upgrade_mode(pattern: &str) -> bool {
    regex::Regex::new(pattern).is_ok_and(|re| re.is_match(UPGRADE_MODE_KEY))
}

/// Raw-spelling preflight, run BEFORE `resolve_command` (which is lossy: it
/// picks one action by priority and silently drops the rest). When any raw
/// operand names the reserved namespace, fail closed on:
/// - conflicting action spellings (`--list --get upgrade.mode`,
///   `--import --get upgrade.mode`, legacy flag + explicit subcommand, …);
/// - actions that take no key operand (`--list`/`--import` with a reserved
///   positional key would silently drop it);
/// - value-pattern forms of key-only legacy actions
///   (`--unset upgrade.mode <pattern>` must not become an unconditional
///   reset, `--get upgrade.mode <pattern>` must not ignore the pattern).
fn upgrade_namespace_raw_preflight(args: &ConfigArgs) -> CliResult<()> {
    let positional_reserved = args.key.as_deref().is_some_and(is_upgrade_namespace_key);
    let subcommand_reserved = matches!(
        &args.command,
        Some(
            ConfigCommand::Set { key, .. }
                | ConfigCommand::Get { key, .. }
                | ConfigCommand::Unset { key, .. }
        ) if is_upgrade_namespace_key(key)
    );
    // With `--get-regexp`, the positional operand is a PATTERN: it reaches the
    // reserved key when it can match it, not when it spells it literally.
    let regexp_reserved =
        args.get_regexp && args.key.as_deref().is_some_and(regexp_reaches_upgrade_mode);
    // `--rename-section <old> <new>` carries its operands in the two
    // positionals; the DESTINATION (valuepattern) must be checked too, or a
    // rename INTO the reserved namespace escapes the router when combined
    // with another action flag.
    let rename_reserved = args.rename_section
        && (args.key.as_deref().is_some_and(is_upgrade_namespace_key)
            || args
                .valuepattern
                .as_deref()
                .is_some_and(is_upgrade_namespace_key));
    if !positional_reserved && !subcommand_reserved && !regexp_reserved && !rename_reserved {
        return Ok(());
    }
    if upgrade_conflicting_action_spelling(args) {
        return Err(upgrade_namespace_usage_error(
            "combining multiple action spellings",
        ));
    }
    if positional_reserved && (args.list || args.import) {
        return Err(upgrade_namespace_usage_error(
            "--list/--import with a reserved key operand",
        ));
    }
    if positional_reserved
        && args.valuepattern.is_some()
        && (args.get || args.get_all || args.unset || args.unset_all)
    {
        return Err(upgrade_namespace_usage_error(
            "a value-pattern operand on get/unset",
        ));
    }
    Ok(())
}

/// A reserved-key operation must be a single, unambiguous spelling. Git-compat
/// legacy flags are resolved by priority order, silently ignoring the rest —
/// acceptable for ordinary keys, but a dropped `--add`/`--unset-all` on the
/// reserved namespace would hide a multivalue/mutation intent, so any
/// combination of action spellings fails closed here.
fn upgrade_conflicting_action_spelling(args: &ConfigArgs) -> bool {
    let mut actions = usize::from(args.command.is_some());
    for flag in [
        args.get,
        args.get_all,
        args.unset,
        args.unset_all,
        args.add,
        args.list,
        args.get_regexp,
        args.import,
        args.remove_section,
        args.rename_section,
    ] {
        if flag {
            actions += 1;
        }
    }
    actions > 1
}

/// Intercept every resolved config command that targets the reserved
/// `upgrade.*` namespace. Returns `None` when the command does not touch the
/// namespace; `Some(result)` when it was fully handled (or rejected).
///
/// Contract (§A.3): only single-value `set`/`get`/`unset` with an explicit
/// `--global` scope are supported; `unset` resets `mode` to `off` and keeps
/// the file; every other spelling (local/system scope, `--add`, `--get-all`,
/// type conversion, encryption, sections, conflicting action-flag combos,
/// `--get-regexp` patterns matching the reserved key) fails closed. `list`
/// and non-matching `--get-regexp` patterns are not intercepted here — they
/// suppress reserved keys read from SQLite and (for `list`) render the
/// file-backed entry instead.
async fn route_upgrade_namespace(
    args: &ConfigArgs,
    cmd: &ResolvedCommand,
    output: &OutputConfig,
) -> Option<CliResult<()>> {
    // Reject ambiguous action spellings before dispatching any reserved-key
    // operation (a silently-dropped legacy flag must not slip through).
    let guard_conflicts = || -> CliResult<()> {
        if upgrade_conflicting_action_spelling(args) {
            return Err(upgrade_namespace_usage_error(
                "combining multiple action spellings",
            ));
        }
        Ok(())
    };
    match cmd {
        ResolvedCommand::Set {
            key,
            value,
            add,
            encrypt,
            plaintext,
            stdin,
            value_type,
            // The reserved-namespace router is fail-closed for every spelling
            // of a write, so it never needs to tell the two set forms apart
            // nor how a read would have been terminated.
            explicit_set: _,
            null: _,
        } if is_upgrade_namespace_key(key) => Some(match guard_conflicts() {
            Err(err) => Err(err),
            Ok(()) => {
                handle_upgrade_set(
                    args,
                    key,
                    value.as_deref(),
                    *add,
                    *encrypt || *plaintext || *stdin,
                    value_type.is_some(),
                    output,
                )
                .await
            }
        }),
        ResolvedCommand::Get {
            key,
            all,
            regexp,
            default,
            null,
            value_type,
            ..
        } if !*regexp && is_upgrade_namespace_key(key) => Some(match guard_conflicts() {
            Err(err) => Err(err),
            Ok(()) => {
                handle_upgrade_get(
                    args,
                    key,
                    *all,
                    default.is_some(),
                    *null,
                    value_type.is_some(),
                    output,
                )
                .await
            }
        }),
        // A regexp that can match the reserved key is a spelling that reaches
        // it — fail closed instead of pretending an empty result (§A.3).
        ResolvedCommand::Get { key, regexp, .. } if *regexp && regexp_reaches_upgrade_mode(key) => {
            Some(Err(upgrade_namespace_usage_error(
                "--get-regexp with a pattern matching the reserved upgrade.mode",
            )))
        }
        ResolvedCommand::Unset { key, all } if is_upgrade_namespace_key(key) => {
            Some(match guard_conflicts() {
                Err(err) => Err(err),
                Ok(()) => handle_upgrade_unset(args, key, *all, output).await,
            })
        }
        ResolvedCommand::RemoveSection { name } if is_upgrade_namespace_key(name) => {
            Some(Err(upgrade_namespace_usage_error("--remove-section")))
        }
        ResolvedCommand::RenameSection { old, new }
            if is_upgrade_namespace_key(old) || is_upgrade_namespace_key(new) =>
        {
            Some(Err(upgrade_namespace_usage_error("--rename-section")))
        }
        _ => None,
    }
}

/// Reject any scope other than an explicit `--global` for reserved keys.
fn require_upgrade_global_scope(args: &ConfigArgs, operation: &str) -> CliResult<()> {
    if args.global {
        return Ok(());
    }
    Err(CliError::command_usage(format!(
        "the reserved upgrade.* namespace is per-user only; use: libra config {operation} --global upgrade.mode"
    ))
    .with_stable_code(StableErrorCode::CliInvalidArguments)
    .with_hint("upgrade settings live in {LIBRA_HOME}/upgrade/settings.json, not in a repository or system config database"))
}

/// Reject reserved keys other than `upgrade.mode`. Case-insensitive but with
/// no whitespace normalization — a padded spelling like `" upgrade.mode"` is
/// still ROUTED here (detection trims, fail-closed) but rejected as unknown.
fn require_upgrade_mode_key(key: &str) -> CliResult<()> {
    if key.eq_ignore_ascii_case(UPGRADE_MODE_KEY) {
        return Ok(());
    }
    Err(CliError::command_usage(format!(
        "unsupported reserved key '{key}': only {UPGRADE_MODE_KEY} is available"
    ))
    .with_stable_code(StableErrorCode::CliInvalidArguments))
}

async fn handle_upgrade_set(
    args: &ConfigArgs,
    key: &str,
    value: Option<&str>,
    add: bool,
    encrypt_or_stdin: bool,
    typed: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    if add {
        return Err(upgrade_namespace_usage_error(
            "--add (multi-value operations)",
        ));
    }
    if encrypt_or_stdin {
        return Err(upgrade_namespace_usage_error(
            "--encrypt/--plaintext/--stdin",
        ));
    }
    if typed {
        return Err(upgrade_namespace_usage_error("--type conversion"));
    }
    require_upgrade_global_scope(args, "set")?;
    require_upgrade_mode_key(key)?;
    let Some(raw) = value else {
        return Err(CliError::command_usage(
            "a value is required: libra config set --global upgrade.mode <auto|manual|off>",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint("to read the current mode, use: libra config get --global upgrade.mode"));
    };
    let Some(mode) = UpgradeMode::parse(raw) else {
        return Err(CliError::command_usage(format!(
            "invalid value '{raw}' for upgrade.mode (allowed: auto, manual, off)"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    };
    write_upgrade_mode(mode).map_err(upgrade_settings_cli_error)?;
    emit_set_ack("set", ConfigScope::Global, UPGRADE_MODE_KEY, false, output)
}

async fn handle_upgrade_get(
    args: &ConfigArgs,
    key: &str,
    all: bool,
    has_default: bool,
    null: bool,
    typed: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    if all {
        return Err(upgrade_namespace_usage_error("--get-all"));
    }
    if typed {
        return Err(upgrade_namespace_usage_error("--type conversion"));
    }
    if has_default {
        return Err(upgrade_namespace_usage_error(
            "--default (a missing upgrade.mode always reads as 'off')",
        ));
    }
    require_upgrade_global_scope(args, "get")?;
    require_upgrade_mode_key(key)?;
    let mode = read_upgrade_mode()
        .map_err(upgrade_settings_cli_error)?
        .unwrap_or(UpgradeMode::Off);
    if output.is_json() {
        let origin = upgrade_settings_path()
            .map(|p| format!("file:{}", p.display()))
            .ok();
        emit_json_data(
            "config",
            &serde_json::json!({
                "action": "get",
                "key": UPGRADE_MODE_KEY,
                "value": mode.as_str(),
                "origin": origin,
                "default_applied": false,
            }),
            output,
        )?;
    } else if !output.quiet {
        if null {
            print!("{mode}\0");
        } else {
            println!("{mode}");
        }
    }
    Ok(())
}

async fn handle_upgrade_unset(
    args: &ConfigArgs,
    key: &str,
    all: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    if all {
        return Err(upgrade_namespace_usage_error("--unset-all"));
    }
    require_upgrade_global_scope(args, "unset")?;
    require_upgrade_mode_key(key)?;
    // §A.3: unset writes `mode=off` and keeps the file.
    write_upgrade_mode(UpgradeMode::Off).map_err(upgrade_settings_cli_error)?;
    if output.is_json() {
        emit_json_data(
            "config",
            &serde_json::json!({
                "action": "unset",
                "scope": "global",
                "key": UPGRADE_MODE_KEY,
                "reset_to": UpgradeMode::Off.as_str(),
            }),
            output,
        )?;
    } else if !output.quiet {
        println!("Unset global: {UPGRADE_MODE_KEY} (mode reset to off)");
    }
    Ok(())
}

/// File-backed `upgrade.mode` entry for `list` output, or `None` when the
/// settings file does not exist. Corrupt files are a hard error, matching
/// `get` (§A.3).
fn upgrade_list_entry(name_only: bool, with_origin: bool) -> CliResult<Option<ConfigListEntry>> {
    let Some(mode) = read_upgrade_mode().map_err(upgrade_settings_cli_error)? else {
        return Ok(None);
    };
    let origin = if with_origin {
        Some(
            upgrade_settings_path()
                .map(|p| format!("file:{}", p.display()))
                .map_err(upgrade_settings_cli_error)?,
        )
    } else {
        None
    };
    Ok(Some(ConfigListEntry {
        key: UPGRADE_MODE_KEY.to_string(),
        value: if name_only {
            None
        } else {
            Some(mode.as_str().to_string())
        },
        origin,
        encrypted: Some(false),
    }))
}

// ─────────────────────────────────────────────────────────────────────────────
// Command resolution (subcommand ↔ flag translation)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
enum ResolvedCommand {
    Set {
        key: String,
        value: Option<String>,
        add: bool,
        encrypt: bool,
        plaintext: bool,
        stdin: bool,
        /// Validate and canonicalize the value to this type before storing
        /// (`--type`/`--bool`/etc. on a set, matching `git config --type`).
        value_type: Option<ConfigValueType>,
        /// The caller spelled out assignment intent (`libra config set …` or
        /// `--add`). The bare `libra config <key>` form does not: for an
        /// ordinary key it is a READ, so an already-encrypted stored value
        /// must not drag it onto the protected-input path.
        explicit_set: bool,
        /// `-z`/`--null`. Only consulted when the bare form degrades to a
        /// read, where the output must match `config get` byte for byte.
        null: bool,
    },
    Get {
        key: String,
        all: bool,
        reveal: bool,
        regexp: bool,
        default: Option<String>,
        /// NUL-terminate output records (`-z`/`--null`).
        null: bool,
        /// Canonicalize the read value to this type (`--type`/`--bool`/etc.).
        value_type: Option<ConfigValueType>,
    },
    List {
        name_only: bool,
        show_origin: bool,
        vault: bool,
        ssh_keys: bool,
        gpg_keys: bool,
        /// NUL-terminate output records (`-z`/`--null`).
        null: bool,
    },
    Unset {
        key: String,
        all: bool,
    },
    /// Remove an entire section and every key under it.
    RemoveSection {
        name: String,
    },
    /// Rename a section, moving all of its keys from `old.*` to `new.*`.
    RenameSection {
        old: String,
        new: String,
    },
    Import,
    Path,
    Edit,
    GenerateSshKey {
        remote: String,
    },
    GenerateGpgKey {
        name: Option<String>,
        email: Option<String>,
        usage: Option<String>,
    },
}

fn resolve_command(args: &ConfigArgs) -> CliResult<ResolvedCommand> {
    let cmd = resolve_command_typed(args)?;

    // `--type`/`--bool`/`--int`/`--path` canonicalize a value when reading and
    // validate/canonicalize it when setting, so they apply only to get and set
    // operations; any other mode is rejected up front (Git silently ignores the
    // flag there, but Libra prefers an explicit usage error).
    if resolve_value_type(args)?.is_some()
        && !matches!(
            cmd,
            ResolvedCommand::Get { .. } | ResolvedCommand::Set { .. }
        )
    {
        return Err(CliError::command_usage(
            "--type/--bool/--int/--path is only valid with --get/--get-all/--get-regexp or when setting a value",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }

    Ok(cmd)
}

fn resolve_command_typed(args: &ConfigArgs) -> CliResult<ResolvedCommand> {
    // The type flag is threaded into get (canonicalize on read) and set
    // (validate/canonicalize on write); applicability to the resolved command is
    // checked by the caller.
    let value_type = resolve_value_type(args)?;

    // If an explicit subcommand was provided, use it directly
    if let Some(ref cmd) = args.command {
        return Ok(match cmd {
            ConfigCommand::Set {
                key,
                value,
                add,
                encrypt,
                plaintext,
                stdin,
            } => ResolvedCommand::Set {
                key: key.clone(),
                value: value.clone(),
                add: *add,
                encrypt: *encrypt,
                plaintext: *plaintext,
                stdin: *stdin,
                value_type,
                explicit_set: true,
                null: args.null,
            },
            ConfigCommand::Get {
                key,
                all,
                reveal,
                regexp,
                default,
            } => ResolvedCommand::Get {
                key: key.clone(),
                all: *all,
                reveal: *reveal,
                regexp: *regexp,
                default: default.clone(),
                null: args.null,
                value_type,
            },
            ConfigCommand::List {
                name_only,
                show_origin,
                vault,
                ssh_keys,
                gpg_keys,
            } => ResolvedCommand::List {
                name_only: *name_only,
                show_origin: *show_origin,
                vault: *vault,
                ssh_keys: *ssh_keys,
                gpg_keys: *gpg_keys,
                null: args.null,
            },
            ConfigCommand::Unset { key, all } => ResolvedCommand::Unset {
                key: key.clone(),
                all: *all,
            },
            ConfigCommand::Import => ResolvedCommand::Import,
            ConfigCommand::Path => ResolvedCommand::Path,
            ConfigCommand::Edit => ResolvedCommand::Edit,
            ConfigCommand::GenerateSshKey { remote } => ResolvedCommand::GenerateSshKey {
                remote: remote.clone(),
            },
            ConfigCommand::GenerateGpgKey { name, email, usage } => {
                ResolvedCommand::GenerateGpgKey {
                    name: name.clone(),
                    email: email.clone(),
                    usage: usage.clone(),
                }
            }
        });
    }

    // Git-compat flag translation
    if args.list {
        return Ok(ResolvedCommand::List {
            name_only: false,
            show_origin: args.show_origin,
            vault: false,
            ssh_keys: false,
            gpg_keys: false,
            null: args.null,
        });
    }
    if args.remove_section {
        let name = args
            .key
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                CliError::from_legacy_string("error: --remove-section requires a <name>")
                    .with_exit_code(2)
            })?;
        if args.valuepattern.is_some() {
            return Err(CliError::from_legacy_string(
                "error: --remove-section takes exactly one <name>",
            )
            .with_exit_code(2));
        }
        return Ok(ResolvedCommand::RemoveSection {
            name: name.to_string(),
        });
    }
    if args.rename_section {
        match (
            args.key.as_deref().filter(|s| !s.is_empty()),
            args.valuepattern.as_deref().filter(|s| !s.is_empty()),
        ) {
            (Some(old), Some(new)) => {
                return Ok(ResolvedCommand::RenameSection {
                    old: old.to_string(),
                    new: new.to_string(),
                });
            }
            _ => {
                return Err(CliError::from_legacy_string(
                    "error: --rename-section requires <old-name> <new-name>",
                )
                .with_exit_code(2));
            }
        }
    }
    if args.import || args.key.as_deref() == Some("import") {
        if args.import && args.key.is_some() {
            return Err(CliError::from_legacy_string(
                "error: `libra config --import` does not accept <key>",
            ));
        }
        return Ok(ResolvedCommand::Import);
    }

    // Check for "edit" positional
    if args.key.as_deref() == Some("edit") {
        return Ok(ResolvedCommand::Edit);
    }
    // Check for "path" positional
    if args.key.as_deref() == Some("path") {
        return Ok(ResolvedCommand::Path);
    }

    // All remaining modes need a key
    let key = args.key.as_deref().ok_or_else(|| {
        CliError::from_legacy_string("error: missing required argument: <key>").with_exit_code(2)
    })?;

    // Validate key format (must contain at least one dot)
    if !key.contains('.') {
        let mut msg = format!("error: key does not contain a section: {key}");
        if key == "init" || key == "clone" {
            msg.push_str(&format!(
                "\n\nhint: `{key}` is a top-level command. Try `libra {key}`."
            ));
        }
        return Err(CliError::from_legacy_string(msg).with_exit_code(1));
    }

    // --default (-d) is only valid with --get, --get-all, or get-regexp
    if args.default.is_some() && !args.get && !args.get_all && !args.get_regexp {
        return Err(CliError::from_legacy_string(
            "error: --default (-d) can only be used with --get, --get-all, or --get-regexp",
        )
        .with_exit_code(2));
    }

    if args.get_regexp {
        return Ok(ResolvedCommand::Get {
            key: key.to_string(),
            all: false,
            reveal: false,
            regexp: true,
            default: args.default.clone(),
            null: args.null,
            value_type,
        });
    }
    if args.get {
        return Ok(ResolvedCommand::Get {
            key: key.to_string(),
            all: false,
            reveal: false,
            regexp: false,
            default: args.default.clone(),
            null: args.null,
            value_type,
        });
    }
    if args.get_all {
        return Ok(ResolvedCommand::Get {
            key: key.to_string(),
            all: true,
            reveal: false,
            regexp: false,
            default: args.default.clone(),
            null: args.null,
            value_type,
        });
    }
    if args.unset {
        return Ok(ResolvedCommand::Unset {
            key: key.to_string(),
            all: false,
        });
    }
    if args.unset_all {
        return Ok(ResolvedCommand::Unset {
            key: key.to_string(),
            all: true,
        });
    }
    if args.add {
        let value = args.valuepattern.as_deref().ok_or_else(|| {
            CliError::from_legacy_string("error: missing required argument: <value>")
                .with_exit_code(2)
        })?;
        return Ok(ResolvedCommand::Set {
            key: key.to_string(),
            value: Some(value.to_string()),
            add: true,
            encrypt: false,
            plaintext: false,
            stdin: false,
            value_type,
            explicit_set: true,
            null: args.null,
        });
    }

    // Default: the bare `libra config <key> [value]` form. With a value it is a
    // set; without one it is a READ for ordinary keys and the interactive
    // secure-assignment path for protected keys (see `handle_set`).
    Ok(ResolvedCommand::Set {
        key: key.to_string(),
        value: args.valuepattern.clone(),
        add: false,
        encrypt: false,
        plaintext: false,
        stdin: false,
        value_type,
        explicit_set: false,
        null: args.null,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler implementations
// ─────────────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn handle_set(
    key: &str,
    value: Option<&str>,
    add: bool,
    encrypt: bool,
    plaintext: bool,
    stdin: bool,
    value_type: Option<ConfigValueType>,
    scope: ConfigScope,
    use_cascade: bool,
    explicit_set: bool,
    null: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    // Validate key format
    if !key.contains('.') {
        return Err(CliError::from_legacy_string(format!(
            "error: key does not contain a section: {key}"
        ))
        .with_exit_code(1));
    }

    // `--encrypt` and `--plaintext` are mutually exclusive. config.md (line 77)
    // classifies this as a CLI usage error (exit 2 in fine mode, 129 in
    // coarse) — route through `command_usage` so the category matches.
    if encrypt && plaintext {
        return Err(CliError::command_usage(
            "--encrypt and --plaintext are mutually exclusive",
        ));
    }

    // `--plaintext` must not be used with vault internal/secret keys.
    // config.md (line 77) classifies this as a validation reject (exit 1 in
    // fine mode). We use `Failure` (coarse 128) with a stable code so the
    // error class is recoverable rather than silently falling through to
    // `InternalInvariant`.
    if plaintext && (is_vault_internal_key(key) || key.starts_with("vault.env.")) {
        return Err(CliError::failure(
            "--plaintext cannot be used with vault internal/secret keys",
        )
        .with_stable_code(StableErrorCode::RepoStateInvalid));
    }

    // System-scope vault preflight: reject any write that would touch the vault
    // BEFORE any `ScopedConfig` access, so a rejected `--system` vault write
    // never creates or touches `/etc/libra/config.db`. The system scope has no
    // vault (its unseal key would be root-owned and either unreadable to users
    // or world-readable). The post-`get_all` guard below covers the rarer
    // encryption-inheritance edge.
    // The entire `vault.*` namespace is vault-related (incl. non-sensitive
    // pubkeys like `vault.signing`/`vault.ssh.*.pubkey`/`vault.gpg.pubkey` that
    // `is_sensitive_key` does not flag), so reject the whole prefix here. Git
    // section names are case-insensitive, so match `Vault.*` too.
    if scope == ConfigScope::System
        && (encrypt
            || key.to_ascii_lowercase().starts_with("vault.")
            || (is_sensitive_key(key) && !plaintext))
    {
        return Err(CliError::command_usage(
            "vault-encrypted secrets are not supported in --system scope",
        )
        .with_hint("use --global or --local for vault.* keys and --encrypt values"));
    }

    // Check encryption state inheritance from existing entries.
    let existing_entries = ScopedConfig::get_all(scope, key).await.map_err(|e| {
        config_read_cli_error(format!(
            "failed to read {} config while checking existing values for key '{}': {e}",
            scope_name(scope),
            key
        ))
    })?;
    let has_encrypted = existing_entries.iter().any(|e| e.encrypted);
    let has_plaintext = existing_entries.iter().any(|e| !e.encrypted);

    // The system scope holds no vault, so an existing encrypted row should never
    // be there. If one is (e.g. a hand-crafted DB), refuse to write the key at
    // all — even with `--plaintext`, since `set_with_conn` would preserve the
    // row's `encrypted=1` flag while storing a new plaintext value. This catches
    // the edge the encrypt-time preflight above cannot (it runs regardless of
    // `--encrypt`/`--plaintext`).
    if scope == ConfigScope::System && has_encrypted {
        return Err(CliError::command_usage(
            "vault-encrypted secrets are not supported in --system scope",
        )
        .with_hint("this key already has an encrypted value; use --global or --local"));
    }

    // Resolve the value
    let resolved_value = if stdin {
        // `--stdin` and a positional value are mutually exclusive (config.md
        // line 144 — usage error, exit 2 fine / 129 coarse).
        if value.is_some() {
            return Err(CliError::command_usage(
                "cannot use both value argument and --stdin",
            ));
        }
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf).map_err(|e| {
            CliError::from_legacy_string(format!("error: failed to read from stdin: {e}"))
        })?;
        // Strip trailing newline (like Git)
        if buf.ends_with('\n') {
            buf.pop();
            if buf.ends_with('\r') {
                buf.pop();
            }
        }
        buf
    } else if let Some(v) = value {
        v.to_string()
    } else {
        // No value provided
        // `has_encrypted` only expresses assignment intent when the caller
        // asked to assign (`config set <key>` / `--add`). On the bare
        // `libra config <key>` form an ordinary key stays a READ even when its
        // stored value happens to be encrypted — otherwise reading it would
        // report "missing value for protected key" instead of `<REDACTED>`.
        let needs_protected_input =
            !plaintext && (encrypt || is_sensitive_key(key) || (has_encrypted && explicit_set));

        if needs_protected_input {
            // Check if interactive mode is available.
            // Also treat the test harness (`LIBRA_TEST=1`) as non-interactive
            // so that `rpassword::read_password()` never blocks a test run.
            let in_test = std::env::var_os(LIBRA_TEST_ENV).is_some();
            if output.is_json() || in_test || !std::io::stdin().is_terminal() {
                return Err(CliError::from_legacy_string(format!(
                    "error: missing value for protected key '{key}' (non-interactive environment)"
                ))
                .with_exit_code(2));
            }
            // Interactive secure input (no echo)
            eprint!("Enter value for {key}: ");
            rpassword::read_password().map_err(|e| {
                CliError::from_legacy_string(format!("error: failed to read input: {e}"))
            })?
        } else {
            // `libra config <key>` with no value is a READ for ordinary keys, matching
            // `git config <key>` (and `libra config get <key>`). Protected keys are
            // handled above: they keep Libra's interactive secure-assignment path, which
            // is an intentional divergence from Git recorded in `COMPATIBILITY.md`.
            return handle_get(
                key,
                false,
                false,
                false,
                None,
                scope,
                use_cascade,
                null,
                value_type,
                output,
            )
            .await;
        }
    };

    // `--type`/`--bool`/`--int`/`--path` on a set validate and canonicalize the
    // value before it is stored (matching `git config --type`: `yes` -> `true`,
    // `1k` -> `1024`, `~/x` -> the expanded path), erroring on a value that is
    // not valid for the type. Canonicalize the logical value before any
    // encryption so the stored secret round-trips to the canonical form.
    let resolved_value = match value_type {
        Some(value_type) => canonicalize_typed_value(&resolved_value, value_type)?,
        None => resolved_value,
    };

    // Determine encryption
    let should_encrypt = if encrypt {
        true
    } else if plaintext {
        false
    } else if has_encrypted {
        true // Inherit encryption from existing entries
    } else {
        is_sensitive_key(key)
    };

    // Same-key-same-state constraint for --add.
    if add && ((should_encrypt && has_plaintext) || (!should_encrypt && has_encrypted)) {
        return Err(CliError::from_legacy_string(
            "error: cannot mix encrypted and plaintext values for the same key",
        ));
    }

    // Encrypt the value if needed
    let store_value = if should_encrypt {
        // Vault-encrypted secrets are not supported in the system scope: its
        // unseal key would live under a root-owned `/etc/libra` path readable by
        // every user, defeating the encryption, and writing it needs root.
        if scope == ConfigScope::System {
            return Err(CliError::command_usage(
                "vault-encrypted secrets are not supported in --system scope",
            )
            .with_hint("use --global or --local for vault.* keys and --encrypt values"));
        }
        let sn = scope_name(scope);
        let unseal_key = match load_unseal_key_for_scope(sn).await {
            Some(key) => key,
            None => {
                // Lazy init
                let key = lazy_init_vault_for_scope(sn).await.map_err(|e| {
                    CliError::from_legacy_string(format!(
                        "error: failed to initialize vault for {sn} scope: {e}"
                    ))
                })?;
                if !output.quiet && !output.is_json() {
                    println!("Initialized vault for {sn} scope");
                }
                key
            }
        };
        let ciphertext = encrypt_token(&unseal_key, resolved_value.as_bytes())
            .map_err(|e| CliError::from_legacy_string(format!("error: encryption failed: {e}")))?;
        hex::encode(ciphertext)
    } else {
        resolved_value.clone()
    };

    if add {
        ScopedConfig::add(scope, key, &store_value, should_encrypt)
            .await
            .map_err(CliError::from_legacy_string)?;
        emit_set_ack("add", scope, key, should_encrypt, output)?;
    } else {
        ScopedConfig::set(scope, key, &store_value, should_encrypt)
            .await
            .map_err(|e| {
                let err = CliError::from_legacy_string(&e);
                if e.contains("values exist") {
                    err.with_exit_code(5)
                } else {
                    err
                }
            })?;
        emit_set_ack("set", scope, key, should_encrypt, output)?;
    }
    Ok(())
}

/// Decrypt a hex-encoded ciphertext from a config value using the vault unseal key.
/// The `scope` parameter determines which unseal key to load (local or global).
async fn decrypt_config_value(hex_value: &str, scope: &str) -> Result<String, String> {
    let unseal_key = load_unseal_key_for_scope(scope)
        .await
        .ok_or_else(|| format!("vault not initialized for {scope} scope — cannot decrypt"))?;
    let ciphertext =
        hex::decode(hex_value).map_err(|e| format!("failed to decode encrypted value: {e}"))?;
    decrypt_token(&unseal_key, &ciphertext).map_err(|e| format!("decryption failed: {e}"))
}

fn config_read_cli_error(message: impl Into<String>) -> CliError {
    CliError::fatal(message)
        .with_stable_code(StableErrorCode::IoReadFailed)
        .with_exit_code(128)
}

fn config_decrypt_cli_error(key: &str, scope_label: &str, error: impl Into<String>) -> CliError {
    CliError::fatal(format!(
        "failed to decrypt value for key '{key}' from {scope_label} config: {}",
        error.into()
    ))
    .with_stable_code(StableErrorCode::RepoStateInvalid)
    .with_exit_code(128)
}

async fn render_get_value(
    entry: &ConfigKvEntry,
    reveal: bool,
    scope: ConfigScope,
    _use_cascade: bool,
) -> CliResult<String> {
    if !entry.encrypted {
        return Ok(entry.value.clone());
    }

    if !reveal || is_vault_internal_key(&entry.key) {
        return Ok("<REDACTED>".to_string());
    }

    let scope_label = scope_name(scope);
    let decrypted = decrypt_config_value(&entry.value, scope_label)
        .await
        .map_err(|e| config_decrypt_cli_error(&entry.key, scope_label, e))?;

    Ok(decrypted)
}

/// Value type for `--type`/`--bool`/`--int`/`--path` canonicalization on read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfigValueType {
    Bool,
    Int,
    Path,
}

/// Resolve the requested read type from `--type=<t>` or a `--bool`/`--int`/
/// `--path` shortcut. Returns `None` when none was requested; errors on an
/// unknown `--type`.
fn resolve_value_type(args: &ConfigArgs) -> CliResult<Option<ConfigValueType>> {
    if args.type_bool {
        return Ok(Some(ConfigValueType::Bool));
    }
    if args.type_int {
        return Ok(Some(ConfigValueType::Int));
    }
    if args.type_path {
        return Ok(Some(ConfigValueType::Path));
    }
    match args.value_type.as_deref() {
        None => Ok(None),
        Some("bool") => Ok(Some(ConfigValueType::Bool)),
        Some("int") => Ok(Some(ConfigValueType::Int)),
        Some("path") => Ok(Some(ConfigValueType::Path)),
        Some(other) => Err(CliError::command_usage(format!(
            "error: unsupported --type '{other}' (expected bool, int, or path)"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)),
    }
}

/// Canonicalize a stored value to the requested type when reading, mirroring
/// `git config --type`. Errors on values that are not valid for the type.
fn canonicalize_typed_value(value: &str, ty: ConfigValueType) -> CliResult<String> {
    match ty {
        ConfigValueType::Bool => {
            // git `git_parse_maybe_bool_text`: true/yes/on/1 → true; an explicit
            // empty value and false/no/off/0 → false (`if (!*value) return 0`).
            // Only a *valueless* key (NULL) is true, but Libra always stores an
            // explicit string, so empty → false. The comparison is on the raw
            // value (no trimming), so a padded " true " is not a valid bool →
            // error, matching Git.
            match value.to_ascii_lowercase().as_str() {
                "true" | "yes" | "on" | "1" => Ok("true".to_string()),
                "false" | "no" | "off" | "0" | "" => Ok("false".to_string()),
                _ => Err(CliError::command_usage(format!(
                    "error: cannot convert value '{value}' to bool"
                ))
                .with_stable_code(StableErrorCode::CliInvalidArguments)),
            }
        }
        ConfigValueType::Int => {
            // git: optional k/m/g (case-insensitive) 1024-based multiplier. The
            // value is parsed without trimming, so surrounding whitespace makes
            // it invalid (the numeric parse rejects it).
            let v = value;
            let (num, mult) = match v.chars().last() {
                Some('k') | Some('K') => (&v[..v.len() - 1], 1024_i64),
                Some('m') | Some('M') => (&v[..v.len() - 1], 1024 * 1024),
                Some('g') | Some('G') => (&v[..v.len() - 1], 1024 * 1024 * 1024),
                _ => (v, 1),
            };
            let base: i64 = num.parse().map_err(|_| {
                CliError::command_usage(format!("error: cannot convert value '{value}' to int"))
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
            })?;
            let scaled = base.checked_mul(mult).ok_or_else(|| {
                CliError::command_usage(format!("error: integer value '{value}' overflows"))
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
            })?;
            Ok(scaled.to_string())
        }
        ConfigValueType::Path => {
            // git --path: expand a leading `~`/`~/` to the home directory.
            // `~user` expansion is not supported (returned unchanged).
            if value == "~"
                && let Some(home) = dirs::home_dir()
            {
                return Ok(home.to_string_lossy().into_owned());
            }
            if let Some(rest) = value.strip_prefix("~/")
                && let Some(home) = dirs::home_dir()
            {
                return Ok(home.join(rest).to_string_lossy().into_owned());
            }
            Ok(value.to_string())
        }
    }
}

/// Apply an optional `--type` canonicalization to a read value.
fn apply_value_type(value: String, ty: Option<ConfigValueType>) -> CliResult<String> {
    match ty {
        Some(t) => canonicalize_typed_value(&value, t),
        None => Ok(value),
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_get(
    key: &str,
    all: bool,
    reveal: bool,
    regexp: bool,
    default: Option<&str>,
    scope: ConfigScope,
    use_cascade: bool,
    null: bool,
    value_type: Option<ConfigValueType>,
    output: &OutputConfig,
) -> CliResult<()> {
    // Block --reveal for vault internal keys on exact-key queries
    if reveal && !regexp && !all && is_vault_internal_key(key) {
        return Err(CliError::from_legacy_string(format!(
            "error: key '{}' is a vault internal credential and cannot be revealed",
            key
        )));
    }

    if regexp {
        // Regex search across all keys
        let mut entries: Vec<(ConfigKvEntry, ConfigScope)> = if use_cascade {
            let mut all_entries = Vec::new();
            for s in ConfigScope::CASCADE_ORDER {
                if s != ConfigScope::Local {
                    let Some(path) = s.get_config_path() else {
                        continue;
                    };
                    if !path.exists() {
                        continue;
                    }
                }
                let scope_entries = match ScopedConfig::get_regexp(s, key).await {
                    Ok(entries) => entries,
                    Err(e) if should_skip_config_scope_read_error(s, &e) => continue,
                    Err(e) => {
                        return Err(config_read_cli_error(format!(
                            "failed to read {} config: {e}",
                            scope_name(s)
                        )));
                    }
                };
                for e in scope_entries {
                    all_entries.push((e, s));
                }
            }
            all_entries
        } else {
            ScopedConfig::get_regexp(scope, key)
                .await
                .map_err(CliError::from_legacy_string)?
                .into_iter()
                .map(|e| (e, scope))
                .collect()
        };
        // Reserved namespace: `upgrade.*` never legitimately lives in SQLite,
        // so a pattern match can only hit stale legacy rows — suppress them
        // (§A.3; exact-key reads are routed to the settings file instead).
        entries.retain(|(e, _)| !is_upgrade_namespace_key(&e.key));

        // Build display values with decryption support
        let mut display_entries = Vec::new();
        for (e, s) in &entries {
            let val = apply_value_type(
                render_get_value(e, reveal, *s, use_cascade).await?,
                value_type,
            )?;
            display_entries.push((e, s, val));
        }

        if output.is_json() {
            emit_json_data(
                "config",
                &serde_json::json!({
                    "action": "get-regexp",
                    "pattern": key,
                    "entries": display_entries.iter().map(|(e, s, val)| serde_json::json!({
                        "key": e.key,
                        "value": val,
                        "origin": scope_name(**s),
                        "encrypted": e.encrypted,
                    })).collect::<Vec<_>>(),
                }),
                output,
            )?;
        } else if !output.quiet {
            for (e, _, val) in &display_entries {
                if null {
                    // `git config -z --get-regexp`: key\nvalue\0 per entry.
                    print!("{}\n{val}\0", e.key);
                } else {
                    println!("{} = {val}", e.key);
                }
            }
        }
        return Ok(());
    }

    if all {
        // Get all values for a specific key
        let entries: Vec<(ConfigKvEntry, ConfigScope)> = if use_cascade {
            get_all_cascaded(key).await.map_err(config_read_cli_error)?
        } else {
            ScopedConfig::get_all(scope, key)
                .await
                .map_err(CliError::from_legacy_string)?
                .into_iter()
                .map(|e| (e, scope))
                .collect()
        };

        if entries.is_empty()
            && let Some(d) = default
        {
            // Canonicalize the default through `--type` like a stored value.
            let d = apply_value_type(d.to_string(), value_type)?;
            if output.is_json() {
                emit_json_data(
                    "config",
                    &serde_json::json!({
                        "action": "get-all",
                        "key": key,
                        "entries": [{"value": d, "origin": serde_json::Value::Null}],
                        "default_applied": true,
                    }),
                    output,
                )?;
            } else if !output.quiet {
                if null {
                    print!("{d}\0");
                } else {
                    println!("{d}");
                }
            }
            return Ok(());
        }

        // Build display values with decryption support
        let mut display_entries = Vec::new();
        for (e, s) in &entries {
            let val = apply_value_type(
                render_get_value(e, reveal, *s, use_cascade).await?,
                value_type,
            )?;
            display_entries.push((e, s, val));
        }

        if output.is_json() {
            emit_json_data(
                "config",
                &serde_json::json!({
                    "action": "get-all",
                    "key": key,
                    "entries": display_entries.iter().map(|(e, s, val)| serde_json::json!({
                        "value": val,
                        "origin": scope_name(**s),
                        "encrypted": e.encrypted,
                    })).collect::<Vec<_>>(),
                    "default_applied": false,
                }),
                output,
            )?;
        } else if !output.quiet {
            for (_, _, val) in &display_entries {
                if null {
                    print!("{val}\0");
                } else {
                    println!("{val}");
                }
            }
        }
    } else {
        // Get single value (last-one-wins)
        let entry: Option<(ConfigKvEntry, ConfigScope)> = if use_cascade {
            get_cascaded(key).await.map_err(config_read_cli_error)?
        } else {
            ScopedConfig::get(scope, key)
                .await
                .map_err(CliError::from_legacy_string)?
                .map(|e| (e, scope))
        };

        let (display_value, default_applied, origin_scope) = match entry {
            Some((ref e, s)) => {
                let val = apply_value_type(
                    render_get_value(e, reveal, s, use_cascade).await?,
                    value_type,
                )?;
                (val, false, Some(s))
            }
            None => {
                if let Some(d) = default {
                    (apply_value_type(d.to_string(), value_type)?, true, None)
                } else {
                    // Spell correction: find closest matching key
                    let all_keys = if use_cascade {
                        let mut keys = Vec::new();
                        for s in ConfigScope::CASCADE_ORDER {
                            if s != ConfigScope::Local {
                                let Some(path) = s.get_config_path() else {
                                    continue;
                                };
                                if !path.exists() {
                                    continue;
                                }
                            }
                            if let Ok(entries) = ScopedConfig::list_all(s).await {
                                for e in entries {
                                    if !keys.contains(&e.key) {
                                        keys.push(e.key);
                                    }
                                }
                            }
                        }
                        keys
                    } else {
                        ScopedConfig::list_all(scope)
                            .await
                            .unwrap_or_default()
                            .into_iter()
                            .map(|e| e.key)
                            .collect()
                    };

                    let mut best_match = None;
                    let mut best_dist = usize::MAX;
                    for k in &all_keys {
                        let dist = levenshtein(key, k);
                        if dist < best_dist && dist <= 3 {
                            best_dist = dist;
                            best_match = Some(k.clone());
                        }
                    }

                    let mut msg = format!("key '{key}' not found in any scope");
                    if let Some(suggestion) = best_match {
                        msg.push_str(&format!("\n\nhint: did you mean '{suggestion}'?"));
                    }
                    msg.push_str("\nhint: use libra config list to see all configured keys");
                    return Err(CliError::failure(msg)
                        .with_stable_code(StableErrorCode::CliInvalidArguments)
                        .with_exit_code(1));
                }
            }
        };

        if output.is_json() {
            emit_json_data(
                "config",
                &serde_json::json!({
                    "action": "get",
                    "key": key,
                    "value": display_value,
                    "origin": origin_scope.map(scope_name),
                    "default_applied": default_applied,
                }),
                output,
            )?;
        } else if !output.quiet {
            if null {
                print!("{display_value}\0");
            } else {
                println!("{display_value}");
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_list(
    name_only: bool,
    show_origin: bool,
    vault: bool,
    ssh_keys: bool,
    gpg_keys: bool,
    scope: ConfigScope,
    use_cascade: bool,
    null: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    // `-z` defines NUL records for standard config key/value output only. The
    // `--ssh-keys` / `--gpg-keys` / `--vault` views are Libra-only formatted
    // summaries with no `key\nvalue\0` mapping, so reject the combination rather
    // than silently ignore `-z`.
    if null && (ssh_keys || gpg_keys || vault) {
        return Err(CliError::command_usage(
            "-z/--null applies to standard config output only, not --ssh-keys/--gpg-keys/--vault",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }

    if ssh_keys {
        let entries = list_ssh_key_entries(scope).await?;
        if output.is_json() {
            emit_json_data(
                "config",
                &serde_json::json!({
                    "action": "list-ssh-keys",
                    "keys": entries,
                    "count": entries.len(),
                }),
                output,
            )?;
        } else if !output.quiet {
            if entries.is_empty() {
                println!("No SSH keys configured.");
            } else {
                println!("SSH keys:");
                for entry in &entries {
                    println!("  {:<10} {}", entry.remote, entry.public_key);
                }
                println!();
                println!("{} keys configured", entries.len());
                println!();
                println!("Tip: use libra config generate-ssh-key --remote <name> to add more");
            }
        }
        return Ok(());
    }

    if gpg_keys {
        let entries = list_gpg_key_entries(scope).await?;
        if output.is_json() {
            emit_json_data(
                "config",
                &serde_json::json!({
                    "action": "list-gpg-keys",
                    "keys": entries,
                    "count": entries.len(),
                }),
                output,
            )?;
        } else if !output.quiet {
            if entries.is_empty() {
                println!("No GPG keys configured.");
            } else {
                println!("GPG keys:");
                for entry in &entries {
                    let signing_suffix = if entry.usage == "signing" && entry.signing_enabled {
                        "  (vault.signing = true)"
                    } else {
                        ""
                    };
                    println!(
                        "  {:<10} {}{}",
                        entry.usage, entry.pubkey_config_key, signing_suffix
                    );
                }
                println!();
                println!("{} keys configured", entries.len());
            }
        }
        return Ok(());
    }

    if vault {
        // List vault.env.* entries across scopes
        let mut entries = Vec::new();
        for s in ConfigScope::CASCADE_ORDER {
            if s != ConfigScope::Local {
                let Some(path) = s.get_config_path() else {
                    continue;
                };
                if !path.exists() {
                    continue;
                }
            }
            if let Ok(scope_entries) = ScopedConfig::get_by_prefix(s, "vault.env.").await {
                for e in scope_entries {
                    let plaintext_warning = if !e.encrypted && is_sensitive_key(&e.key) {
                        " [PLAINTEXT]"
                    } else {
                        ""
                    };
                    entries.push(ConfigListEntry {
                        key: e.key,
                        value: Some(if e.encrypted {
                            "<REDACTED>".to_string()
                        } else {
                            format!("{}{plaintext_warning}", e.value)
                        }),
                        origin: Some(scope_name(s).to_string()),
                        encrypted: Some(e.encrypted),
                    });
                }
            }
        }

        if output.is_json() {
            emit_json_data(
                "config",
                &serde_json::json!({
                    "action": "list-vault",
                    "entries": entries,
                    "encrypted_count": entries.len(),
                }),
                output,
            )?;
        } else if !output.quiet {
            if entries.is_empty() {
                println!("No vault environment variables configured.");
            } else {
                println!("Vault environment variables (cascade):");
                for e in &entries {
                    let origin = e.origin.as_deref().unwrap_or("?");
                    let val = e.value.as_deref().unwrap_or("");
                    println!("  {:<8} {} = {}  (encrypted)", origin, e.key, val);
                }
                println!("\n{} encrypted entries", entries.len());
                println!("\nNext steps:");
                println!("  - add:     libra config set vault.env.<ENV_VAR_NAME>");
                println!("  - remove:  libra config unset vault.env.<name>");
            }
        }
        return Ok(());
    }

    if show_origin {
        // Show all entries with scope labels
        let mut entries = Vec::new();
        for s in ConfigScope::CASCADE_ORDER {
            if s != ConfigScope::Local {
                let Some(path) = s.get_config_path() else {
                    continue;
                };
                if !path.exists() {
                    continue;
                }
            }
            if let Ok(scope_entries) = ScopedConfig::list_all(s).await {
                for e in scope_entries {
                    // Reserved namespace: never render `upgrade.*` rows from
                    // SQLite — the file-backed entry below is the only source
                    // of truth (§A.3 "不得再从 SQLite 输出第二份").
                    if is_upgrade_namespace_key(&e.key) {
                        continue;
                    }
                    let plaintext_warning = if !e.encrypted && is_sensitive_key(&e.key) {
                        " [PLAINTEXT]"
                    } else {
                        ""
                    };
                    entries.push(ConfigListEntry {
                        key: e.key.clone(),
                        value: if name_only {
                            None
                        } else if e.encrypted {
                            Some("<REDACTED>".to_string())
                        } else {
                            Some(format!("{}{plaintext_warning}", e.value))
                        },
                        origin: if show_origin {
                            Some(scope_name(s).to_string())
                        } else {
                            None
                        },
                        encrypted: Some(e.encrypted),
                    });
                }
            }
        }

        // File-backed reserved entry (origin `file:{path}`, §A.3). It belongs
        // to the global (per-user) scope, so an explicit `--local`/`--system`
        // list must not render it.
        if (use_cascade || scope == ConfigScope::Global)
            && let Some(entry) = upgrade_list_entry(name_only, show_origin)?
        {
            entries.push(entry);
        }

        if output.is_json() {
            emit_json_data(
                "config",
                &serde_json::json!({
                    "action": "list",
                    "scope": if show_origin { "all" } else { scope_name(scope) },
                    "cascade": use_cascade,
                    "entries": entries,
                    "count": entries.len(),
                }),
                output,
            )?;
        } else if !output.quiet {
            for e in &entries {
                match (&e.origin, &e.value) {
                    // `git config -z`: origin\0key\nvalue\0 (origin omitted when
                    // not requested; value omitted with --name-only).
                    (Some(origin), Some(val)) if null => print!("{origin}\0{}\n{val}\0", e.key),
                    (Some(origin), None) if null => print!("{origin}\0{}\0", e.key),
                    (None, Some(val)) if null => print!("{}\n{val}\0", e.key),
                    (None, None) if null => print!("{}\0", e.key),
                    (Some(origin), Some(val)) => println!("  {:<8} {} = {val}", origin, e.key),
                    (Some(origin), None) => println!("  {:<8} {}", origin, e.key),
                    (None, Some(val)) => println!("{}={val}", e.key),
                    (None, None) => println!("{}", e.key),
                }
            }
        }
    } else {
        // Single scope list
        let scope_entries = ScopedConfig::list_all(scope)
            .await
            .map_err(CliError::from_legacy_string)?;

        let mut entries: Vec<ConfigListEntry> = scope_entries
            .into_iter()
            // Reserved namespace: suppress any legacy SQLite `upgrade.*` rows.
            .filter(|e| !is_upgrade_namespace_key(&e.key))
            .map(|e| {
                let plaintext_warning = if !e.encrypted && is_sensitive_key(&e.key) {
                    " [PLAINTEXT]"
                } else {
                    ""
                };
                ConfigListEntry {
                    key: e.key.clone(),
                    value: if name_only {
                        None
                    } else if e.encrypted {
                        Some("<REDACTED>".to_string())
                    } else {
                        Some(format!("{}{plaintext_warning}", e.value))
                    },
                    origin: None,
                    encrypted: Some(e.encrypted),
                }
            })
            .collect();

        // The reserved `upgrade.mode` belongs to the global (per-user) scope.
        if scope == ConfigScope::Global
            && let Some(entry) = upgrade_list_entry(name_only, false)?
        {
            entries.push(entry);
        }

        if output.is_json() {
            emit_json_data(
                "config",
                &serde_json::json!({
                    "action": "list",
                    "scope": scope_name(scope),
                    "entries": entries,
                    "count": entries.len(),
                }),
                output,
            )?;
        } else if !output.quiet {
            for e in &entries {
                match &e.value {
                    // `git config -z --list`: key\nvalue\0 (key\0 with --name-only).
                    Some(val) if null => print!("{}\n{val}\0", e.key),
                    None if null => print!("{}\0", e.key),
                    Some(val) => println!("{}={val}", e.key),
                    None => println!("{}", e.key),
                }
            }
        }
    }
    Ok(())
}

async fn list_ssh_key_entries(scope: ConfigScope) -> CliResult<Vec<ConfigSshKeyEntry>> {
    let mut entries = ScopedConfig::get_by_prefix(scope, "vault.ssh.")
        .await
        .map_err(CliError::from_legacy_string)?
        .into_iter()
        .filter_map(|entry| {
            let remote = entry
                .key
                .strip_prefix("vault.ssh.")?
                .strip_suffix(".pubkey")?;
            let mut parts = entry.value.split_whitespace();
            let key_type = parts.next().unwrap_or("ssh").to_string();
            let _material = parts.next()?;
            let key_id = parts.collect::<Vec<_>>().join(" ");
            Some(ConfigSshKeyEntry {
                remote: remote.to_string(),
                key_type,
                public_key: entry.value,
                key_id: (!key_id.is_empty()).then_some(key_id),
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.remote.cmp(&right.remote));
    Ok(entries)
}

async fn list_gpg_key_entries(scope: ConfigScope) -> CliResult<Vec<ConfigGpgKeyEntry>> {
    let mut entries = ScopedConfig::list_all(scope)
        .await
        .map_err(CliError::from_legacy_string)?
        .into_iter()
        .filter_map(|entry| {
            let usage = match entry.key.as_str() {
                "vault.gpg.pubkey" | "vault.gpg_pubkey" => "signing".to_string(),
                key if key.starts_with("vault.gpg.") && key.ends_with(".pubkey") => key
                    .strip_prefix("vault.gpg.")?
                    .strip_suffix(".pubkey")?
                    .to_string(),
                _ => return None,
            };
            Some((usage, entry.key))
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries.dedup_by(|left, right| left.0 == right.0);

    let signing_enabled = ScopedConfig::get(scope, "vault.signing")
        .await
        .map_err(CliError::from_legacy_string)?
        .map(|entry| entry.value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    Ok(entries
        .into_iter()
        .map(|(usage, pubkey_config_key)| ConfigGpgKeyEntry {
            signing_enabled: usage == "signing" && signing_enabled,
            usage,
            key_type: "PGP 2048".to_string(),
            pubkey_config_key,
        })
        .collect())
}

async fn handle_unset(
    key: &str,
    all: bool,
    scope: ConfigScope,
    output: &OutputConfig,
) -> CliResult<()> {
    let count = if all {
        ScopedConfig::unset_all(scope, key)
            .await
            .map_err(CliError::from_legacy_string)?
    } else {
        ScopedConfig::unset(scope, key).await.map_err(|e| {
            let err = CliError::from_legacy_string(&e);
            if e.contains("values exist") {
                err.with_exit_code(5)
            } else {
                err
            }
        })?
    };

    if output.is_json() {
        emit_json_data(
            "config",
            &serde_json::json!({
                "action": if all { "unset-all" } else { "unset" },
                "scope": scope_name(scope),
                "key": key,
                "removed_count": count,
            }),
            output,
        )?;
    } else if !output.quiet {
        if all && count > 1 {
            println!(
                "Unset {}: {} (removed {} values)",
                scope_name(scope),
                key,
                count
            );
        } else {
            println!("Unset {}: {}", scope_name(scope), key);
        }
    }
    Ok(())
}

/// Map a transactional write failure to a user-facing error (exit 128).
fn config_write_cli_error(message: impl Into<String>) -> CliError {
    CliError::fatal(message)
        .with_stable_code(StableErrorCode::IoWriteFailed)
        .with_exit_code(128)
}

/// Whether `key` belongs to git section `section`, using Git's section /
/// subsection identity rather than a raw prefix. A fully-qualified key splits
/// as `section.[subsection.]name` (section = before the FIRST dot, name = after
/// the LAST dot, subsection = anything between). `<section>` is likewise
/// `section` (bare) or `section.subsection`. So `--remove-section branch`
/// matches `branch.autosetup` but NOT `branch.feature.remote` (that key is in
/// subsection `feature`, addressed as `branch.feature`) — matching Git, and
/// avoiding deleting unrelated subsections.
fn key_in_section(key: &str, section: &str) -> bool {
    let Some((key_sec, key_rest)) = key.split_once('.') else {
        return false;
    };
    let (target_sec, target_sub) = match section.split_once('.') {
        Some((s, sub)) => (s, Some(sub)),
        None => (section, None),
    };
    if key_sec != target_sec {
        return false;
    }
    match key_rest.rsplit_once('.') {
        // key = section.subsection.name → belongs only if the subsection matches.
        Some((key_sub, _name)) => target_sub == Some(key_sub),
        // key = section.name (no subsection) → belongs only to the bare section.
        None => target_sub.is_none(),
    }
}

/// Distinct keys belonging to `section`, read transactionally. Candidates are
/// narrowed by the `<section>.` SQL prefix, then filtered to exact members with
/// [`key_in_section`].
async fn section_member_keys<C: sea_orm::ConnectionTrait>(
    txn: &C,
    section: &str,
) -> CliResult<Vec<String>> {
    let prefix = format!("{section}.");
    let entries = ConfigKv::get_by_prefix_with_conn(txn, &prefix)
        .await
        .map_err(|e| config_read_cli_error(format!("failed to read config: {e}")))?;
    let mut keys: Vec<String> = entries
        .into_iter()
        .filter(|e| key_in_section(&e.key, section))
        .map(|e| e.key)
        .collect();
    keys.sort();
    keys.dedup();
    Ok(keys)
}

/// `--remove-section <name>`: delete every key in section `<name>` (Git
/// section semantics — see [`key_in_section`]) in one transaction. A section
/// with no keys is "No such section" (exit 128), matching
/// `git config --remove-section`.
async fn handle_remove_section(
    name: &str,
    scope: ConfigScope,
    output: &OutputConfig,
) -> CliResult<()> {
    let conn = ScopedConfig::get_connection(scope)
        .await
        .map_err(config_read_cli_error)?;
    // Begin first so the existence check and the deletes are one atomic unit.
    let txn = conn
        .begin()
        .await
        .map_err(|e| config_write_cli_error(format!("failed to start config transaction: {e}")))?;

    let keys = section_member_keys(&txn, name).await?;
    if keys.is_empty() {
        return Err(
            CliError::from_legacy_string(format!("error: No such section: {name}"))
                .with_exit_code(128),
        );
    }

    let mut removed = 0usize;
    for key in &keys {
        removed += ConfigKv::unset_all_with_conn(&txn, key)
            .await
            .map_err(|e| config_write_cli_error(format!("failed to remove '{key}': {e}")))?;
    }
    txn.commit()
        .await
        .map_err(|e| config_write_cli_error(format!("failed to commit config transaction: {e}")))?;

    if output.is_json() {
        emit_json_data(
            "config",
            &serde_json::json!({
                "action": "remove-section",
                "scope": scope_name(scope),
                "section": name,
                "removed_count": removed,
            }),
            output,
        )?;
    } else if !output.quiet {
        println!("Removed section {}: {name}", scope_name(scope));
    }
    Ok(())
}

/// `--rename-section <old> <new>`: move every key in section `<old>` to the
/// matching key in section `<new>` (value and encryption flag preserved) in one
/// transaction. A missing source is "No such section" (exit 128). Renaming onto
/// the same name, or onto a destination section that already has keys, is
/// rejected — the latter avoids ambiguous merges and encrypted/plaintext
/// flag inheritance, so every destination write lands on a fresh key.
async fn handle_rename_section(
    old: &str,
    new: &str,
    scope: ConfigScope,
    output: &OutputConfig,
) -> CliResult<()> {
    if old == new {
        return Err(CliError::from_legacy_string(format!(
            "error: source and destination sections are identical: {old}"
        ))
        .with_exit_code(2));
    }

    let conn = ScopedConfig::get_connection(scope)
        .await
        .map_err(config_read_cli_error)?;
    let txn = conn
        .begin()
        .await
        .map_err(|e| config_write_cli_error(format!("failed to start config transaction: {e}")))?;

    // Read source members transactionally (exact section semantics). Use the
    // full entries (value + encrypted) in insertion order for the re-add.
    let old_prefix = format!("{old}.");
    let source: Vec<ConfigKvEntry> = ConfigKv::get_by_prefix_with_conn(&txn, &old_prefix)
        .await
        .map_err(|e| config_read_cli_error(format!("failed to read config: {e}")))?
        .into_iter()
        .filter(|e| key_in_section(&e.key, old))
        .collect();
    if source.is_empty() {
        return Err(
            CliError::from_legacy_string(format!("error: No such section: {old}"))
                .with_exit_code(128),
        );
    }

    // Refuse to write into a destination section that already exists, so every
    // re-added key is fresh (preserving the source's exact value + encrypted
    // flag, and avoiding silent multi-value merges).
    if !section_member_keys(&txn, new).await?.is_empty() {
        return Err(CliError::from_legacy_string(format!(
            "error: destination section already exists: {new}"
        ))
        .with_exit_code(128));
    }

    // System scope holds no vault: refuse a rename that would either carry an
    // encrypted source row into it or land a key under a vault/secret namespace
    // (which direct `set --system` also rejects).
    if scope == ConfigScope::System {
        for e in &source {
            let name = e.key.strip_prefix(&old_prefix).unwrap_or(&e.key);
            let new_key = format!("{new}.{name}");
            if e.encrypted
                || new_key.to_ascii_lowercase().starts_with("vault.")
                || is_sensitive_key(&new_key)
            {
                return Err(CliError::command_usage(
                    "vault-encrypted secrets are not supported in --system scope",
                )
                .with_hint(
                    "rename into a vault/secret namespace is rejected in --system; use --global or --local",
                ));
            }
        }
    }

    for e in &source {
        // Exact members all begin with `{old}.`; the remainder is the key name
        // under the section (which itself may contain dots for nested names).
        let name = e.key.strip_prefix(&old_prefix).unwrap_or(&e.key);
        let new_key = format!("{new}.{name}");
        ConfigKv::add_with_conn(&txn, &new_key, &e.value, e.encrypted)
            .await
            .map_err(|err| config_write_cli_error(format!("failed to write '{new_key}': {err}")))?;
    }
    let mut old_keys: Vec<String> = source.iter().map(|e| e.key.clone()).collect();
    old_keys.sort();
    old_keys.dedup();
    for key in &old_keys {
        ConfigKv::unset_all_with_conn(&txn, key)
            .await
            .map_err(|err| config_write_cli_error(format!("failed to remove '{key}': {err}")))?;
    }
    txn.commit()
        .await
        .map_err(|e| config_write_cli_error(format!("failed to commit config transaction: {e}")))?;

    if output.is_json() {
        emit_json_data(
            "config",
            &serde_json::json!({
                "action": "rename-section",
                "scope": scope_name(scope),
                "old": old,
                "new": new,
                "moved_count": old_keys.len(),
            }),
            output,
        )?;
    } else if !output.quiet {
        println!("Renamed section {}: {old} -> {new}", scope_name(scope));
    }
    Ok(())
}

async fn handle_import(scope: ConfigScope, output: &OutputConfig) -> CliResult<()> {
    // Import auto-encrypts sensitive keys (`is_sensitive_key`), but the system
    // scope does not support the vault, so importing into it could silently
    // store a plaintext value flagged as encrypted. Reject it up front for the
    // same reason `--encrypt`/`vault.*` writes are rejected in this scope.
    if scope == ConfigScope::System {
        return Err(CliError::command_usage(
            "config import is not supported in --system scope",
        )
        .with_hint(
            "import would encrypt sensitive keys, which the system scope does not support; import into --global or --local",
        ));
    }

    let summary = import_git_config(scope)
        .await
        .map_err(CliError::from_legacy_string)?;

    if output.is_json() {
        emit_json_data(
            "config",
            &serde_json::json!({
                "action": "import",
                "source": format!("git-{}", summary.scope),
                "target_scope": summary.scope,
                "imported": summary.imported,
                "skipped_duplicates": summary.skipped_duplicates,
                "auto_encrypted": summary.auto_encrypted,
                "collapsed_multivalue_warnings": summary.collapsed_multivalue_warnings,
                "ignored_invalid": summary.ignored_invalid,
                "ignored_reserved": summary.ignored_reserved,
            }),
            output,
        )?;
    } else if !output.quiet {
        print_import_summary(&summary);
    }
    Ok(())
}

async fn handle_path(scope: ConfigScope, output: &OutputConfig) -> CliResult<()> {
    let path = match scope {
        ConfigScope::Local => {
            let storage = try_get_storage_path(None).map_err(|_| {
                CliError::from_legacy_string(
                    "error: not a libra repository (or any parent up to /)\n\nhint: use --global to read/write user-level config without a repository\nhint: use libra init to create a repository here",
                )
            })?;
            storage.join(DATABASE)
        }
        ConfigScope::Global | ConfigScope::System => scope.get_config_path().ok_or_else(|| {
            CliError::from_legacy_string(format!(
                "error: could not determine {} config path",
                scope_name(scope)
            ))
        })?,
    };

    let exists = path.exists();

    if output.is_json() {
        emit_json_data(
            "config",
            &serde_json::json!({
                "action": "path",
                "scope": scope_name(scope),
                "path": path.to_string_lossy(),
                "exists": exists,
            }),
            output,
        )?;
    } else if !output.quiet {
        println!("{}", path.display());
    }
    Ok(())
}

async fn handle_generate_ssh_key(
    remote: &str,
    scope: ConfigScope,
    output: &OutputConfig,
) -> CliResult<()> {
    reject_global_key_generation(scope, "generate-ssh-key")?;

    // Validate remote name. config.md "generate-ssh-key" spec classifies
    // this as a CLI usage error (`error: invalid remote name '<name>': only
    // [a-zA-Z0-9_-] allowed`), so we must surface it via
    // `CliError::command_usage` (which maps to the `Cli` category → exit
    // 129 in coarse mode, 2 in fine mode) rather than the generic
    // `from_legacy_string` path that collapses to `Failure` / exit 128.
    if !remote
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        || remote.is_empty()
        || remote.len() > 64
    {
        return Err(CliError::command_usage(format!(
            "invalid remote name '{remote}': only [a-zA-Z0-9_-] allowed, 1-64 chars"
        )));
    }

    // Verify remote exists. Missing remote is a Fatal failure (the user's
    // input is well-formed but the resource does not exist at execution
    // time), classified under the Repo category — exit 128 in coarse mode
    // matches the pre-existing behaviour from the legacy `from_legacy_string`
    // routing this branch used to follow.
    let remote_exists = ConfigKv::remote_config(remote)
        .await
        .map_err(|e| CliError::from_legacy_string(e.to_string()))?;
    if remote_exists.is_none() {
        return Err(CliError::failure(format!(
            "remote '{remote}' not found, add it first with libra remote add"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid));
    }

    // Get vault root dir and unseal key
    let storage = try_get_storage_path(None)
        .map_err(|_| CliError::from_legacy_string("error: not a libra repository"))?;

    let unseal_key = match load_unseal_key_for_scope("local").await {
        Some(key) => key,
        None => {
            let key = lazy_init_vault_for_scope("local").await.map_err(|e| {
                CliError::from_legacy_string(format!(
                    "error: failed to initialize vault for local scope: {e}"
                ))
            })?;
            if !output.quiet {
                println!("Initialized vault for local scope");
            }
            key
        }
    };

    // Get user name for key ID
    let user_name = ConfigKv::get("user.name")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
        .unwrap_or_else(|| "Libra User".to_string());

    // Generate key pair via vault (returns both pub and priv)
    let (public_key, private_key) = generate_ssh_key_pair(&storage, &unseal_key, &user_name)
        .await
        .map_err(|e| {
            CliError::from_legacy_string(format!("error: SSH key generation failed: {e}"))
        })?;

    // Store public key plaintext in config_kv
    let pubkey_key = format!("vault.ssh.{remote}.pubkey");
    let _ = ConfigKv::set(&pubkey_key, &public_key, false).await;

    // Store private key encrypted in config_kv (vault-backed, no persistent file)
    let privkey_key = format!("vault.ssh.{remote}.privkey");
    let encrypted_privkey = encrypt_token(&unseal_key, private_key.as_bytes()).map_err(|e| {
        CliError::from_legacy_string(format!("error: failed to encrypt SSH private key: {e}"))
    })?;
    let _ = ConfigKv::set(&privkey_key, &hex::encode(encrypted_privkey), true).await;

    if output.is_json() {
        emit_json_data(
            "config",
            &serde_json::json!({
                "action": "generate-ssh-key",
                "remote": remote,
                "type": "RSA",
                "bits": 3072,
                "public_key": public_key,
                "pubkey_config_key": pubkey_key,
                "privkey_config_key": privkey_key,
                "storage": "vault-encrypted",
            }),
            output,
        )?;
    } else if !output.quiet {
        println!("Generated SSH key for remote '{remote}':");
        println!("  Type:       RSA 3072");
        println!("  Public key: {public_key}");
        println!();
        println!("Stored:");
        println!("  public key:  {pubkey_key} (in config)");
        println!("  private key: {privkey_key} (vault-encrypted, temp file on use)");
        println!();
        println!("Next steps:");
        println!("  - add to GitHub:  copy the public key above to your GitHub SSH settings");
        println!("  - push:           libra push {remote} main");
    }
    Ok(())
}

async fn handle_generate_gpg_key(
    name: Option<&str>,
    email: Option<&str>,
    usage: Option<&str>,
    scope: ConfigScope,
    output: &OutputConfig,
) -> CliResult<()> {
    reject_global_key_generation(scope, "generate-gpg-key")?;

    let usage = match usage.unwrap_or("signing") {
        "signing" => "signing",
        "encrypt" => "encrypt",
        other => {
            return Err(CliError::from_legacy_string(format!(
                "error: invalid value '{other}' for '--usage <KIND>' (expected 'signing' or 'encrypt')"
            )));
        }
    };
    let is_signing = usage == "signing";

    let storage = try_get_storage_path(None)
        .map_err(|_| CliError::from_legacy_string("error: not a libra repository"))?;

    let unseal_key = match load_unseal_key_for_scope("local").await {
        Some(key) => key,
        None => {
            let key = lazy_init_vault_for_scope("local").await.map_err(|e| {
                CliError::from_legacy_string(format!(
                    "error: failed to initialize vault for local scope: {e}"
                ))
            })?;
            if !output.quiet {
                println!("Initialized vault for local scope");
            }
            key
        }
    };

    let user_name = name
        .map(String::from)
        .unwrap_or_else(|| "Libra User".to_string());

    let user_email = email
        .map(String::from)
        .unwrap_or_else(|| "user@libra.local".to_string());

    let public_key = generate_pgp_key(&storage, &unseal_key, &user_name, &user_email)
        .await
        .map_err(|e| {
            CliError::from_legacy_string(format!("error: GPG key generation failed: {e}"))
        })?;

    // Store pubkey under usage-specific dotted key
    let pubkey_config_key = if is_signing {
        "vault.gpg.pubkey".to_string()
    } else {
        format!("vault.gpg.{usage}.pubkey")
    };
    let _ = ConfigKv::set(&pubkey_config_key, &public_key, false).await;

    // Only enable vault.signing for signing usage
    if is_signing {
        let _ = ConfigKv::set("vault.signing", "true", false).await;
    }

    if output.is_json() {
        emit_json_data(
            "config",
            &serde_json::json!({
                "action": "generate-gpg-key",
                "usage": usage,
                "type": "PGP",
                "bits": 2048,
                "user": format!("{user_name} <{user_email}>"),
                "pubkey_config_key": pubkey_config_key,
                "signing_enabled": is_signing,
            }),
            output,
        )?;
    } else if !output.quiet {
        if is_signing {
            println!("Generated GPG key:");
        } else {
            println!("Generated GPG key (usage: {usage}):");
        }
        println!("  Type:    PGP 2048-bit");
        println!("  User:    {user_name} <{user_email}>");
        println!("  Valid:   10 years");
        println!();
        println!("Stored:");
        println!("  public key: {pubkey_config_key} (in config)");
        if is_signing {
            println!();
            println!("Tip: commit signing is now enabled (vault.signing = true)");
        }
    }
    Ok(())
}

fn reject_global_key_generation(scope: ConfigScope, command: &str) -> CliResult<()> {
    if scope == ConfigScope::Local {
        return Ok(());
    }

    Err(CliError::command_usage(format!(
        "{command} only supports local scope; --global key generation is not supported yet"
    ))
    .with_hint("run without --global to generate a repository-local key"))
}

// ─────────────────────────────────────────────────────────────────────────────
// Import from Git
// ─────────────────────────────────────────────────────────────────────────────

/// Known multi-value keys that should use --add semantics during import.
const KNOWN_MULTI_VALUE_PREFIXES: &[&str] = &[
    "remote.", // remote.*.fetch, remote.*.push, remote.*.pushurl
    "branch.", // branch.*.merge
    "url.",    // url.*.insteadOf, url.*.pushInsteadOf
    "http.",   // http.*.extraHeader
];

const KNOWN_MULTI_VALUE_KEYS: &[&str] = &["credential.helper"];

fn is_known_multi_value_key(key: &str) -> bool {
    if KNOWN_MULTI_VALUE_KEYS.contains(&key) {
        return true;
    }
    for prefix in KNOWN_MULTI_VALUE_PREFIXES {
        if let Some(suffix) = key.strip_prefix(prefix)
            && let Some((_name, leaf)) = suffix.rsplit_once('.')
            && matches!(
                leaf,
                "fetch"
                    | "push"
                    | "pushurl"
                    | "merge"
                    | "insteadOf"
                    | "pushInsteadOf"
                    | "extraHeader"
            )
        {
            return true;
        }
    }
    false
}

async fn import_git_config(scope: ConfigScope) -> Result<ConfigImportSummary, String> {
    let git_flag = match scope {
        ConfigScope::Local => "--local",
        ConfigScope::Global => "--global",
        ConfigScope::System => "--system",
    };

    let mut git_args = vec!["config", git_flag, "--list", "-z"];
    if matches!(scope, ConfigScope::Global | ConfigScope::System) {
        git_args.push("--no-includes");
    }

    let output = Command::new("git")
        .args(&git_args)
        .output()
        .map_err(|e| format!("failed to run `git config`: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let scope_label = scope_name(scope);
        let mut msg = format!("error: failed to import Git {scope_label} config");
        if !stderr.is_empty() {
            let detail = stderr.strip_prefix("fatal: ").unwrap_or(&stderr);
            msg.push_str(&format!("\n  {detail}"));
        }
        if scope == ConfigScope::Local {
            msg.push_str("\n\nhint: Run this command inside a Git repository, or use `--global`.");
        }
        return Err(msg);
    }

    let mut imported = 0usize;
    let mut skipped = 0usize;
    let mut ignored_invalid = 0usize;
    let mut ignored_reserved = 0usize;
    let mut auto_encrypted = 0usize;
    let mut collapsed_warnings = 0usize;

    // Track multi-value collapse for non-known keys
    let mut last_value_wins: std::collections::HashMap<String, (String, usize)> =
        std::collections::HashMap::new();

    // First pass: collect all entries
    let mut all_entries: Vec<(String, String)> = Vec::new();
    for entry in output
        .stdout
        .split(|b| *b == 0)
        .filter(|chunk| !chunk.is_empty())
    {
        let raw = String::from_utf8_lossy(entry);
        let (key_raw, value) = match raw.split_once('\n') {
            Some((k, v)) => (k.trim().to_string(), v.to_string()),
            None => {
                // Implicit boolean value
                let trimmed = raw.trim().to_string();
                if trimmed.contains('.') {
                    (trimmed, "true".to_string())
                } else {
                    ignored_invalid += 1;
                    continue;
                }
            }
        };

        // Validate key format
        if !key_raw.contains('.') {
            ignored_invalid += 1;
            continue;
        }
        // Reserved namespace (§A.3): never import `upgrade.*` into SQLite —
        // upgrade settings are managed only via `{LIBRA_HOME}/upgrade/settings.json`.
        if is_upgrade_namespace_key(&key_raw) {
            ignored_reserved += 1;
            continue;
        }
        all_entries.push((key_raw, value));
    }

    // Process entries
    for (key, value) in &all_entries {
        if is_known_multi_value_key(key) {
            // Multi-value: use add semantics, skip exact duplicates
            let existing = ScopedConfig::get_all(scope, key).await?;
            if existing.iter().any(|e| &e.value == value) {
                skipped += 1;
                continue;
            }
            let should_encrypt = is_sensitive_key(key);
            let store_value = if should_encrypt {
                if let Some(unseal_key) = load_unseal_key_for_scope(scope_name(scope)).await {
                    if let Ok(ct) = encrypt_token(&unseal_key, value.as_bytes()) {
                        hex::encode(ct)
                    } else {
                        value.clone()
                    }
                } else {
                    value.clone()
                }
            } else {
                value.clone()
            };
            ScopedConfig::add(scope, key, &store_value, should_encrypt).await?;
            imported += 1;
            if should_encrypt {
                auto_encrypted += 1;
            }
        } else {
            // Single-value: track for last-one-wins
            let count = last_value_wins
                .entry(key.clone())
                .or_insert_with(|| (String::new(), 0));
            count.0 = value.clone();
            count.1 += 1;
        }
    }

    // Apply last-one-wins entries
    for (key, (value, count)) in &last_value_wins {
        if *count > 1 {
            collapsed_warnings += 1;
            emit_warning(format!(
                "key '{key}' has {count} values in Git config, only last value kept (not in known multi-value list)"
            ));
        }

        let existing = ScopedConfig::get(scope, key).await?;
        if existing.as_ref().map(|e| &e.value) == Some(value) {
            skipped += 1;
            continue;
        }
        let should_encrypt = is_sensitive_key(key);
        let store_value = if should_encrypt {
            if let Some(unseal_key) = load_unseal_key_for_scope(scope_name(scope)).await {
                if let Ok(ct) = encrypt_token(&unseal_key, value.as_bytes()) {
                    hex::encode(ct)
                } else {
                    value.clone()
                }
            } else {
                value.clone()
            }
        } else {
            value.clone()
        };
        ScopedConfig::set(scope, key, &store_value, should_encrypt).await?;
        imported += 1;
        if should_encrypt {
            auto_encrypted += 1;
        }
    }

    if ignored_invalid > 0 {
        emit_warning(format!(
            "ignored {ignored_invalid} unsupported Git config entries"
        ));
    }
    if ignored_reserved > 0 {
        emit_warning(format!(
            "ignored {ignored_reserved} reserved upgrade.* entries (manage the upgrade mode with: libra config set --global upgrade.mode <auto|manual|off>)"
        ));
    }

    Ok(ConfigImportSummary {
        scope: scope_name(scope),
        imported,
        skipped_duplicates: skipped,
        ignored_invalid,
        ignored_reserved,
        auto_encrypted,
        collapsed_multivalue_warnings: collapsed_warnings,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Cascade helpers
// ─────────────────────────────────────────────────────────────────────────────

async fn get_cascaded(key: &str) -> Result<Option<(ConfigKvEntry, ConfigScope)>, String> {
    for scope in ConfigScope::CASCADE_ORDER {
        if scope != ConfigScope::Local {
            let Some(path) = scope.get_config_path() else {
                continue;
            };
            if !path.exists() {
                continue;
            }
        }
        match ScopedConfig::get(scope, key).await {
            Ok(Some(v)) => return Ok(Some((v, scope))),
            Ok(None) => continue,
            Err(e) if should_skip_config_scope_read_error(scope, &e) => continue,
            Err(e) => {
                return Err(format!("failed to read {} config: {e}", scope_name(scope)));
            }
        }
    }
    Ok(None)
}

async fn get_all_cascaded(key: &str) -> Result<Vec<(ConfigKvEntry, ConfigScope)>, String> {
    let mut out = Vec::new();
    for scope in ConfigScope::CASCADE_ORDER {
        if scope != ConfigScope::Local {
            let Some(path) = scope.get_config_path() else {
                continue;
            };
            if !path.exists() {
                continue;
            }
        }
        match ScopedConfig::get_all(scope, key).await {
            Ok(values) => {
                for v in values {
                    out.push((v, scope));
                }
            }
            Err(e) if should_skip_config_scope_read_error(scope, &e) => continue,
            Err(e) => return Err(format!("failed to read {} config: {e}", scope_name(scope))),
        }
    }
    Ok(out)
}

fn should_skip_config_scope_read_error(scope: ConfigScope, error: &str) -> bool {
    // Out-of-date schemas are now upgraded automatically on connect; the only
    // surviving incompatibility is a global/system config DB whose schema is
    // newer than this binary supports — skip that scope rather than failing the
    // read. A system config that is present but unreadable (e.g. permissions) is
    // also skipped so a stray `/etc/libra/config.db` cannot break every read.
    match scope {
        ConfigScope::Global => error.contains("is newer than this Libra binary supports"),
        ConfigScope::System => true,
        ConfigScope::Local => false,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Output helpers
// ─────────────────────────────────────────────────────────────────────────────

fn scope_name(scope: ConfigScope) -> &'static str {
    match scope {
        ConfigScope::Local => "local",
        ConfigScope::Global => "global",
        ConfigScope::System => "system",
    }
}

fn get_scope(args: &ConfigArgs) -> ConfigScope {
    if args.system {
        ConfigScope::System
    } else if args.global {
        ConfigScope::Global
    } else {
        ConfigScope::Local
    }
}

fn has_explicit_scope(args: &ConfigArgs) -> bool {
    args.local || args.global || args.system
}

fn emit_set_ack(
    action: &str,
    scope: ConfigScope,
    key: &str,
    encrypted: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        emit_json_data(
            "config",
            &serde_json::json!({
                "action": action,
                "scope": scope_name(scope),
                "key": key,
                "encrypted": encrypted,
            }),
            output,
        )?;
    } else if !output.quiet {
        let scope_label = scope_name(scope);
        let enc_label = if encrypted { " (encrypted)" } else { "" };
        let action_label = if action == "add" { "Added" } else { "Set" };
        println!("{action_label} {scope_label}{enc_label}: {key}");
    }
    Ok(())
}

fn print_import_summary(summary: &ConfigImportSummary) {
    if summary.imported > 0 {
        println!(
            "Imported {} entries from Git {} config → libra {} config",
            summary.imported, summary.scope, summary.scope
        );
    } else {
        println!(
            "No new entries to import from Git {} config.",
            summary.scope
        );
    }
    let mut details = Vec::new();
    if summary.skipped_duplicates > 0 {
        details.push(format!("{} duplicates", summary.skipped_duplicates));
    }
    if summary.ignored_invalid > 0 {
        details.push(format!("{} invalid keys", summary.ignored_invalid));
    }
    if !details.is_empty() {
        println!("  skipped: {}", details.join(", "));
    }
    if summary.auto_encrypted > 0 {
        println!(
            "  encrypted: {} sensitive key{} auto-encrypted",
            summary.auto_encrypted,
            if summary.auto_encrypted == 1 { "" } else { "s" }
        );
    }
    if summary.collapsed_multivalue_warnings > 0 {
        println!(
            "  warnings: {} multi-value keys collapsed",
            summary.collapsed_multivalue_warnings
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod args_tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn scope_flags_are_mutually_exclusive() {
        let args = ConfigArgs::try_parse_from([
            "config",
            "--global",
            "--local",
            "set",
            "user.name",
            "test",
        ]);
        assert!(args.is_err());
    }

    #[test]
    fn subcommand_set_parses() {
        let args = ConfigArgs::try_parse_from(["config", "set", "user.name", "John"]).unwrap();
        assert!(matches!(args.command, Some(ConfigCommand::Set { .. })));
    }

    #[test]
    fn subcommand_get_parses() {
        let args = ConfigArgs::try_parse_from(["config", "get", "user.name"]).unwrap();
        assert!(matches!(args.command, Some(ConfigCommand::Get { .. })));
    }

    #[test]
    fn subcommand_list_parses() {
        let args = ConfigArgs::try_parse_from(["config", "list"]).unwrap();
        assert!(matches!(args.command, Some(ConfigCommand::List { .. })));
    }

    #[test]
    fn git_compat_list_flag() {
        let args = ConfigArgs::try_parse_from(["config", "-l"]).unwrap();
        assert!(args.list);
    }
}
