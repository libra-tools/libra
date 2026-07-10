//! Fetch command to negotiate with remotes, download pack data, update
//! remote-tracking refs, and honor `--depth` shallow options and `--tags` /
//! `--no-tags`. (Prune is not yet implemented — see
//! `docs/development/commands/fetch.md`.)

use std::{
    collections::{BTreeSet, HashSet},
    fs,
    io::{self, Error as IoError, Write},
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, Instant, SystemTime},
};

use clap::Parser;
use git_internal::{
    errors::GitError,
    hash::{HashKind, ObjectHash, get_hash_kind},
    internal::object::commit::Commit,
};
use indicatif::ProgressBar;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set, TransactionError,
    TransactionTrait,
};
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::io::StreamReader;
use url::Url;

use crate::{
    command::{
        index_pack, load_object,
        remote::{
            RemotePruneEntry, classify_stale_tracking_branches, remote_advertised_branch_names,
        },
    },
    git_protocol::ServiceType::{self, UploadPack},
    internal::{
        branch::Branch,
        config::{ConfigKv, ConfigKvEntry, RemoteConfig},
        db::get_db_conn_instance,
        head::Head,
        model::reference as ref_model,
        protocol::{
            DiscRef, DiscoveryResult, FetchStream, ProtocolClient,
            git_client::GitClient,
            https_client::HttpsClient,
            local_client::LocalClient,
            set_wire_hash_kind,
            ssh_client::{SshClient, is_ssh_spec},
        },
        reflog::{HEAD, Reflog, ReflogAction, ReflogContext},
        tag::{self, TagObject},
        vault::{decrypt_token, load_unseal_key},
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{
            OutputConfig, ProgressMode, ProgressPreference, ProgressReporter, emit_json_data,
        },
        path, util,
        util::try_get_storage_path,
    },
};

const FETCH_EXAMPLES: &str = "\
EXAMPLES:
    libra fetch                            Fetch the current branch's upstream
    libra fetch origin                     Fetch from a specific remote
    libra fetch origin main                Fetch only one branch from a remote
    libra fetch --all                      Fetch every configured remote
    libra fetch origin --depth 1           Shallow fetch (latest commit only)
    libra fetch --all --depth 3            Shallow fetch across all remotes
    libra fetch origin --tags              Fetch all tags into refs/tags/* as well
    libra fetch origin --dry-run           Preview ref updates without downloading
    libra fetch origin --porcelain         Machine-readable per-ref update lines
    libra fetch origin -v                  Announce the remote on stderr
    libra fetch origin --notes             Also fetch the file-dependency graph (local Libra source)
    libra --json fetch origin              Structured JSON output for agents";

pub(crate) enum RemoteClient {
    Http(HttpsClient),
    Local(LocalClient),
    Git(GitClient),
    Ssh(SshClient),
}

impl RemoteClient {
    /// Create a `RemoteClient` from a URL spec, optionally providing the
    /// logical remote name so that vault-backed SSH keys can be resolved
    /// via `vault.ssh.<remote>.privkey`.
    pub(crate) fn from_spec_with_remote(spec: &str, remote: Option<&str>) -> Result<Self, String> {
        // Check for SSH-style URLs first (before Url::parse which doesn't handle SCP-style)
        if is_ssh_spec(spec) {
            let client = configure_ssh_client(SshClient::from_ssh_spec(spec)?, remote)?;
            return Ok(Self::Ssh(client));
        }

        if let Ok(mut url) = Url::parse(spec) {
            // Convert Windows path like "D:\test\1" to "file:///d:/test/1"
            if url.scheme().len() == 1 {
                url = Url::parse(&format!("file:///{}:{}", url.scheme(), url.path()))
                    .map_err(|_| format!("invalid Windows file url: {spec}"))?;
            }
            match url.scheme() {
                "http" | "https" => Ok(Self::Http(HttpsClient::from_url(&url))),
                "file" => {
                    let path = url
                        .to_file_path()
                        .map_err(|_| format!("invalid file url: {spec}"))?;
                    let client = LocalClient::from_path(path)
                        .map_err(|e| format!("invalid local repository '{}': {}", spec, e))?;
                    Ok(Self::Local(client))
                }
                "git" => {
                    if url.host_str().is_none() {
                        return Err(format!("invalid git url '{spec}': missing host"));
                    }
                    Ok(Self::Git(GitClient::from_url(&url)))
                }
                "ssh" => {
                    let client = configure_ssh_client(SshClient::from_ssh_spec(spec)?, remote)?;
                    Ok(Self::Ssh(client))
                }
                other => Err(format!("unsupported remote scheme '{other}'")),
            }
        } else {
            let normalized = spec.trim_end_matches('/');
            let normalized = if normalized.is_empty() && spec.starts_with('/') {
                "/"
            } else {
                normalized
            };
            let client = LocalClient::from_path(normalized)
                .map_err(|e| format!("invalid local repository '{}': {}", spec, e))?;
            Ok(Self::Local(client))
        }
    }

    pub(crate) fn with_network_timeouts(
        self,
        connect_timeout: Duration,
        idle_timeout: Duration,
    ) -> Result<Self, String> {
        match self {
            Self::Http(client) => Ok(Self::Http(
                client.with_timeouts(connect_timeout, idle_timeout)?,
            )),
            Self::Ssh(client) => Ok(Self::Ssh(client.with_idle_timeout(idle_timeout))),
            Self::Git(client) => Ok(Self::Git(
                client.with_network_timeouts(connect_timeout, idle_timeout),
            )),
            // Local remotes read from disk — no network timeouts apply.
            other => Ok(other),
        }
    }

    /// Apply the connect/idle timeouts resolved from the environment, config, and
    /// built-in defaults for this remote. A no-op for local remotes.
    pub(crate) fn with_resolved_fetch_timeouts(self, remote: Option<&str>) -> Result<Self, String> {
        let is_local = matches!(self, Self::Local(_));
        if is_local {
            return Ok(self);
        }
        let connect = resolve_fetch_timeout(
            remote,
            "connectTimeout",
            "LIBRA_FETCH_CONNECT_TIMEOUT_MS",
            Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS),
        );
        let idle = resolve_fetch_timeout(
            remote,
            "idleTimeout",
            "LIBRA_FETCH_IDLE_TIMEOUT_MS",
            Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS),
        );
        let first_byte = resolve_fetch_timeout(
            remote,
            "firstByteTimeout",
            "LIBRA_FETCH_FIRST_BYTE_TIMEOUT_MS",
            Duration::from_secs(DEFAULT_FIRST_BYTE_TIMEOUT_SECS),
        );
        let client = self.with_network_timeouts(connect, idle)?;
        // The first-byte timeout only applies to the git:// path today; http/ssh
        // bound the first response through their own read timeouts.
        Ok(match client {
            Self::Git(git) => Self::Git(git.with_first_byte_timeout(first_byte)),
            other => other,
        })
    }

    pub(crate) async fn discovery_reference(
        &self,
        service: ServiceType,
    ) -> Result<DiscoveryResult, GitError> {
        match self {
            RemoteClient::Http(client) => client.discovery_reference(service).await,
            RemoteClient::Local(client) => client.discovery_reference(service).await,
            RemoteClient::Git(client) => client.discovery_reference(service).await,
            RemoteClient::Ssh(client) => client.discovery_reference(service).await,
        }
    }

    async fn fetch_objects(
        &self,
        have: &[String],
        want: &[String],
        shallow: &[String],
        depth: Option<usize>,
    ) -> Result<FetchStream, IoError> {
        match self {
            RemoteClient::Http(client) => client.fetch_objects(have, want, shallow, depth).await,
            RemoteClient::Local(client) => client.fetch_objects(have, want, shallow, depth).await,
            RemoteClient::Git(client) => client.fetch_objects(have, want, shallow, depth).await,
            RemoteClient::Ssh(client) => client.fetch_objects(have, want, shallow, depth).await,
        }
    }
}

const SSH_KEY_TEMP_FILE_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

fn configure_ssh_client(mut client: SshClient, remote: Option<&str>) -> Result<SshClient, String> {
    if let Err(error) = cleanup_expired_vault_ssh_temp_files() {
        tracing::warn!("failed to clean up expired SSH key temp files: {error}");
    }
    if let Some(mode) = load_ssh_host_key_checking_mode() {
        client = client.with_strict_host_key_checking(mode)?;
    }
    // Try to load vault SSH key for authentication.
    // Priority:
    // 1. vault.ssh.<remote>.privkey (vault-encrypted, decrypted to temp file)
    // 2. Legacy filesystem path ~/.libra/ssh-keys/<repo-id>/id_ed25519
    // 3. No explicit key (fall back to system default SSH agent/keys)
    if let Some(key_file) = try_load_vault_ssh_key_for_remote(remote)? {
        client = client.with_temp_key_file(key_file);
    } else if let Some(key_path) = try_load_legacy_ssh_key_path() {
        client = client.with_key_path(key_path);
    }
    Ok(client)
}

/// Try to load SSH private key for a specific remote from vault config.
///
/// Reads `vault.ssh.<remote>.privkey` from config, decrypts it, writes
/// to a secure temporary file, and keeps that file alive for the lifetime
/// of the SSH client. On abnormal process termination, the 24h GC pass will
/// clean up stale `.tmp` files under `~/.libra/tmp/`.
fn try_load_vault_ssh_key_for_remote(
    remote: Option<&str>,
) -> Result<Option<tempfile::NamedTempFile>, String> {
    let Some(remote) = remote else {
        return Ok(None);
    };

    // Only try vault key lookup inside a Libra repository.
    if try_get_storage_path(None).is_err() {
        return Ok(None);
    }

    let privkey_key = format!("vault.ssh.{remote}.privkey");
    let Some(entry) = load_config_entry_sync(&privkey_key)? else {
        return Ok(None);
    };

    if !entry.encrypted {
        return Err(format!(
            "vault SSH private key '{privkey_key}' must be encrypted"
        ));
    }

    // Decrypt the private key using the vault unseal key.
    let unseal_key = load_vault_unseal_key_sync()?
        .ok_or_else(|| format!("failed to load vault unseal key for remote '{remote}'"))?;
    let ciphertext = hex::decode(&entry.value)
        .map_err(|e| format!("failed to decode vault SSH private key '{privkey_key}': {e}"))?;
    let private_key = decrypt_token(&unseal_key, &ciphertext)
        .map_err(|e| format!("failed to decrypt vault SSH private key '{privkey_key}': {e}"))?;

    // Write to a secure temporary file in ~/.libra/tmp/
    let tmp_dir = ensure_vault_ssh_tmp_dir()?;
    let mut tmp_file = tempfile::Builder::new()
        .prefix("ssh-key-")
        .suffix(".tmp")
        .tempfile_in(&tmp_dir)
        .map_err(|e| {
            format!(
                "failed to create temporary SSH key file in '{}': {e}",
                tmp_dir.display()
            )
        })?;
    tmp_file.write_all(private_key.as_bytes()).map_err(|e| {
        format!(
            "failed to write temporary SSH key file '{}': {e}",
            tmp_file.path().display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp_file.path(), std::fs::Permissions::from_mode(0o600)).map_err(
            |e| {
                format!(
                    "failed to set permissions on temporary SSH key file '{}': {e}",
                    tmp_file.path().display()
                )
            },
        )?;
    }

    Ok(Some(tmp_file))
}

/// Load a full config entry (including the `encrypted` flag) synchronously.
fn load_config_entry_sync(dotted_key: &str) -> Result<Option<ConfigKvEntry>, String> {
    use crate::internal::config::ConfigKv;

    fn read_entry_sync(dotted_key: &str) -> Result<Option<ConfigKvEntry>, String> {
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| format!("failed to create tokio runtime for config read: {e}"))?;
        // `get_best_effort` returns an actionable `Err` (instead of panicking)
        // when the repository database cannot be opened — e.g. an enclosing
        // repo whose schema is out of date.
        rt.block_on(ConfigKv::get_best_effort(dotted_key))
            .map_err(|e| format!("failed to read config key '{dotted_key}': {e}"))
    }

    let key = dotted_key.to_string();
    match tokio::runtime::Handle::try_current() {
        Ok(_) => std::thread::scope(|s| {
            s.spawn(|| read_entry_sync(&key))
                .join()
                .map_err(|_| format!("failed to join config read thread for key '{key}'"))?
        }),
        Err(_) => read_entry_sync(&key),
    }
}

/// Load the vault unseal key synchronously.
fn load_vault_unseal_key_sync() -> Result<Option<Vec<u8>>, String> {
    fn read_unseal_key_sync() -> Result<Option<Vec<u8>>, String> {
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| format!("failed to create tokio runtime for vault read: {e}"))?;
        Ok(rt.block_on(load_unseal_key()))
    }

    match tokio::runtime::Handle::try_current() {
        Ok(_) => std::thread::scope(|s| {
            s.spawn(read_unseal_key_sync)
                .join()
                .map_err(|_| "failed to join vault read thread".to_string())?
        }),
        Err(_) => read_unseal_key_sync(),
    }
}

fn resolve_home_directory() -> Result<PathBuf, String> {
    #[cfg(windows)]
    let env_keys = ["USERPROFILE", "HOME"];
    #[cfg(not(windows))]
    let env_keys = ["HOME", "USERPROFILE"];

    for key in env_keys {
        if let Some(value) = std::env::var_os(key)
            && !value.is_empty()
        {
            return Ok(PathBuf::from(value));
        }
    }

    dirs::home_dir().ok_or_else(|| "cannot determine home directory".to_string())
}

fn ensure_vault_ssh_tmp_dir() -> Result<PathBuf, String> {
    let home = resolve_home_directory()?;
    let tmp_dir = home.join(".libra").join("tmp");
    std::fs::create_dir_all(&tmp_dir).map_err(|e| {
        format!(
            "failed to create SSH temp directory '{}': {e}",
            tmp_dir.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_dir, std::fs::Permissions::from_mode(0o700)).map_err(
            |e| {
                format!(
                    "failed to set permissions on SSH temp directory '{}': {e}",
                    tmp_dir.display()
                )
            },
        )?;
    }
    Ok(tmp_dir)
}

fn cleanup_expired_vault_ssh_temp_files() -> Result<usize, String> {
    let home = match dirs::home_dir() {
        Some(home) => home,
        None => return Ok(0),
    };
    cleanup_expired_vault_ssh_temp_files_in(&home.join(".libra").join("tmp"), SystemTime::now())
}

fn cleanup_expired_vault_ssh_temp_files_in(
    tmp_dir: &Path,
    now: SystemTime,
) -> Result<usize, String> {
    if !tmp_dir.exists() {
        return Ok(0);
    }

    let entries = fs::read_dir(tmp_dir).map_err(|e| {
        format!(
            "failed to read SSH temp directory '{}': {e}",
            tmp_dir.display()
        )
    })?;

    let mut removed = 0;
    for entry in entries {
        let entry = entry.map_err(|e| {
            format!(
                "failed to iterate SSH temp directory '{}': {e}",
                tmp_dir.display()
            )
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("failed to inspect SSH temp entry '{}': {e}", path.display()))?;
        if !file_type.is_file() {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("tmp") {
            continue;
        }

        let metadata = entry.metadata().map_err(|e| {
            format!(
                "failed to read metadata for SSH temp entry '{}': {e}",
                path.display()
            )
        })?;
        let modified = metadata.modified().map_err(|e| {
            format!(
                "failed to read modification time for SSH temp entry '{}': {e}",
                path.display()
            )
        })?;
        let age = now.duration_since(modified).unwrap_or_default();
        if age < SSH_KEY_TEMP_FILE_MAX_AGE {
            continue;
        }

        fs::remove_file(&path).map_err(|e| {
            format!(
                "failed to remove expired SSH temp file '{}': {e}",
                path.display()
            )
        })?;
        removed += 1;
    }

    Ok(removed)
}

/// Try to load SSH key from the legacy filesystem path
/// `~/.libra/ssh-keys/<repo-id>/id_ed25519`.
fn try_load_legacy_ssh_key_path() -> Option<String> {
    // Only try vault key lookup inside a Libra repository.
    if try_get_storage_path(None).is_err() {
        return None;
    }

    let repo_id = load_repo_id_sync()?;
    let home = dirs::home_dir()?;
    let key_path = home
        .join(".libra")
        .join("ssh-keys")
        .join(repo_id)
        .join("id_ed25519");

    if key_path.exists() {
        Some(key_path.to_string_lossy().to_string())
    } else {
        None
    }
}

fn load_repo_id_sync() -> Option<String> {
    load_config_sync("libra", None, "repoid")
}

/// Load host key checking mode from env/config for SSH transport.
///
/// Precedence:
/// 1) `LIBRA_SSH_STRICT_HOST_KEY_CHECKING`
/// 2) repo config `ssh.strictHostKeyChecking`
fn load_ssh_host_key_checking_mode() -> Option<String> {
    if let Ok(raw) = std::env::var("LIBRA_SSH_STRICT_HOST_KEY_CHECKING") {
        let mode = raw.trim();
        if !mode.is_empty() {
            return Some(mode.to_string());
        }
    }

    use crate::utils::util;
    if util::try_get_storage_path(None).is_err() {
        return None;
    }
    load_config_sync("ssh", None, "strictHostKeyChecking")
}

/// Default connect timeout for a network fetch (seconds).
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 30;
/// Default idle (per-read) timeout for a network fetch (seconds).
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 60;
/// Default first-byte timeout for a network fetch (seconds) — the wait from
/// sending the `want` list to the first `NAK` / pack byte.
const DEFAULT_FIRST_BYTE_TIMEOUT_SECS: u64 = 30;

/// Resolve one fetch timeout, in precedence order:
///   1. the `LIBRA_FETCH_*_MS` environment variable (milliseconds);
///   2. the `fetch.<remote>.<key>` then `fetch.<key>` config value (whole seconds);
///   3. the supplied built-in default.
///
/// An unparseable OR non-positive (`0`) value at any source is ignored — it
/// falls through to the *next* source (not straight to the default) — so a typo
/// or a `0` can never leave a fetch with a zero-duration timeout, and a bad
/// remote-scoped value never masks a valid un-scoped `fetch.<key>`.
fn resolve_fetch_timeout(
    remote: Option<&str>,
    config_key: &str,
    env_var: &str,
    default: Duration,
) -> Duration {
    // A whole-seconds config value is valid only when it parses AND is positive.
    let parse_secs = |raw: Option<String>| -> Option<Duration> {
        raw.and_then(|value| value.trim().parse::<u64>().ok())
            .filter(|&secs| secs > 0)
            .map(Duration::from_secs)
    };

    // 1. env (milliseconds).
    if let Ok(raw) = std::env::var(env_var)
        && let Ok(ms) = raw.trim().parse::<u64>()
        && ms > 0
    {
        return Duration::from_millis(ms);
    }
    // 2. remote-scoped config `fetch.<remote>.<key>` (seconds), validated on its own.
    if let Some(remote) = remote
        && let Some(duration) = parse_secs(load_config_sync("fetch", Some(remote), config_key))
    {
        return duration;
    }
    // 3. un-scoped config `fetch.<key>` (seconds).
    if let Some(duration) = parse_secs(load_config_sync("fetch", None, config_key)) {
        return duration;
    }
    default
}

fn load_config_sync(configuration: &str, name: Option<&str>, key: &str) -> Option<String> {
    use crate::internal::config::ConfigKv;

    let dotted_key = match name {
        Some(n) => format!("{configuration}.{n}.{key}"),
        None => format!("{configuration}.{key}"),
    };

    // `get_best_effort` never panics when the (possibly *enclosing*) repository
    // database is missing or its schema is out of date; it returns an `Err`
    // that we log and swallow here, so transport setup degrades to "no config
    // value" instead of dumping a panic to stderr during `clone`/`fetch`.
    fn read_value_sync(dotted_key: &str) -> Option<String> {
        let rt = tokio::runtime::Runtime::new().ok()?;
        match rt.block_on(ConfigKv::get_best_effort(dotted_key)) {
            Ok(entry) => entry.map(|e| e.value),
            Err(err) => {
                tracing::debug!("skipping config read for '{dotted_key}': {err}");
                None
            }
        }
    }

    match tokio::runtime::Handle::try_current() {
        Ok(_) => std::thread::scope(|s| {
            s.spawn(|| read_value_sync(&dotted_key))
                .join()
                .ok()
                .flatten()
        }),
        Err(_) => read_value_sync(&dotted_key),
    }
}

#[derive(Parser, Debug)]
#[command(after_help = FETCH_EXAMPLES)]
pub struct FetchArgs {
    /// Repository to fetch from
    pub repository: Option<String>,

    /// Refspec to fetch, usually a branch name
    #[clap(requires("repository"))]
    pub refspec: Option<String>,

    /// Fetch all remotes.
    #[clap(long, short, conflicts_with("repository"))]
    pub all: bool,

    /// Limit fetching to the specified number of commits from the tip of each remote branch
    #[clap(long, value_name = "N")]
    pub depth: Option<usize>,

    /// Show what would be fetched without downloading objects or writing any
    /// refs, reflog, FETCH_HEAD, or shallow metadata.
    #[clap(long = "dry-run")]
    pub dry_run: bool,

    /// Append fetched ref records to `.libra/FETCH_HEAD` instead of overwriting
    /// it. Long-only: `-a` is reserved for `--all` (Git's `-a` is `--append`).
    #[clap(long)]
    pub append: bool,

    /// Print extra diagnostics (the remote being contacted) to stderr, leaving
    /// the stdout result contract unchanged.
    #[clap(long, short = 'v')]
    pub verbose: bool,

    /// Print a machine-readable, single-space-separated line per ref update:
    /// `<flag> <old-oid> <new-oid> <local-ref>`. Mutually exclusive with `--json`.
    #[clap(long)]
    pub porcelain: bool,

    /// Allow updates that are not fast-forward and overwrite (clobber) a local
    /// tag that points elsewhere. Without it, conflicting tags are kept.
    #[clap(long, short = 'f')]
    pub force: bool,

    /// Fetch every tag from the remote into `refs/tags/*` (in addition to the
    /// selected branches). Overrides the default auto-follow and
    /// `remote.<name>.tagOpt`.
    #[clap(long, overrides_with = "no_tags")]
    pub tags: bool,

    /// Do not fetch any tags (not even tags reachable from fetched commits).
    /// Overrides the default auto-follow and an earlier `--tags`.
    #[clap(long = "no-tags", overrides_with = "tags")]
    pub no_tags: bool,

    /// Do not run a repacking/gc pass after fetching. Accepted for Git parity
    /// and is a no-op: Libra's fetch never triggers an automatic gc, so there
    /// is nothing to disable.
    #[clap(long = "no-auto-gc")]
    pub no_auto_gc: bool,

    /// Do not show the progress meter (the "Receiving objects" spinner /
    /// remote progress) on stderr, matching `git fetch --no-progress`.
    #[clap(long = "no-progress")]
    pub no_progress: bool,

    /// Before fetching, delete any remote-tracking ref under
    /// `refs/remotes/<remote>/*` that the remote no longer advertises (a
    /// `refs/heads/*` or `refs/mr/*` ref). Reuses `remote prune`'s stale
    /// classification. With `--dry-run`, the stale refs are reported but not
    /// deleted. Mutually exclusive with `--no-prune` on a last-one-wins basis
    /// (matching Git). Local branches, tags, and other remotes are never
    /// touched.
    #[clap(short = 'p', long = "prune", overrides_with = "no_prune")]
    pub prune: bool,

    /// Do not prune remote-tracking refs that no longer exist on the remote.
    /// This is the default. Pass `--prune`/`-p` to enable pruning; when both are
    /// given, the last one on the command line wins (Git semantics).
    #[clap(long = "no-prune", overrides_with = "prune")]
    pub no_prune: bool,

    /// Also fetch the file-dependency graph (`refs/notes/deps`, lore.md 3.2) from
    /// the remote. Default OFF (Git never auto-fetches notes). v1 travels notes
    /// only from a local Libra source over the local protocol; a network or plain
    /// Git remote emits an honest "not supported yet" warning and fetches no
    /// graph (see `_compatibility.md` D17). Persist the opt-in per remote with
    /// `remote.<name>.fetchNotesDeps=true`.
    #[clap(long = "notes")]
    pub notes: bool,
}

/// How tags are handled for a fetch, resolved per-remote from CLI flags then
/// `remote.<name>.tagOpt` then the Git default (auto-follow).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagFetchMode {
    /// Fetch no tags at all (`--no-tags` / `tagOpt = --no-tags`).
    NoTags,
    /// Auto-follow: persist tags whose objects/targets are present after the
    /// branch fetch (Git's default; `include-tag` brings annotated tag objects).
    AutoFollow,
    /// Fetch every advertised tag (`--tags` / `tagOpt = --tags`).
    All,
}

#[derive(Debug, Clone, Serialize)]
pub struct FetchRefUpdate {
    pub remote_ref: String,
    pub old_oid: Option<String>,
    pub new_oid: String,
    /// True when the update was not a fast-forward (a branch that moved
    /// non-linearly, or a tag that was force-clobbered).
    #[serde(default)]
    pub forced: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FetchRepositoryResult {
    pub remote: String,
    pub url: String,
    pub refs_updated: Vec<FetchRefUpdate>,
    pub objects_fetched: usize,
    /// Bytes received in the fetch pack stream (the `.pack` payload size). Zero
    /// when nothing was transferred (already up to date / nothing to fetch).
    pub bytes_received: usize,
    /// Stale remote-tracking refs removed by `--prune`/`-p` (or, with
    /// `--dry-run`, the refs that *would* be removed). Empty unless pruning was
    /// requested. Serialized only when non-empty so a plain fetch keeps its
    /// original JSON shape.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pruned: Vec<FetchPruneEntry>,
}

/// A remote-tracking ref removed (or, in `--dry-run`, slated for removal) by
/// `fetch --prune`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FetchPruneEntry {
    /// Full local ref name, e.g. `refs/remotes/origin/feature`.
    pub remote_ref: String,
    /// Display form `<remote>/<branch>`, e.g. `origin/feature`.
    pub branch: String,
    /// The object id the ref pointed at before deletion, when it could be read.
    /// Populated for the porcelain old-oid column and JSON audit output.
    pub old_oid: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FetchOutput {
    pub all: bool,
    pub requested_remote: Option<String>,
    pub refspec: Option<String>,
    pub remotes: Vec<FetchRepositoryResult>,
}

/// Typed classification for [`FetchError::InvalidRemoteSpec`] so that callers
/// can map each sub-category to a distinct stable error code without parsing
/// the `reason` string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteSpecErrorKind {
    /// The local path does not exist.
    MissingLocalRepo,
    /// The local path exists but is not a valid libra/git repository.
    InvalidLocalRepo,
    /// The URL is syntactically malformed.
    MalformedUrl,
    /// The URL scheme is not supported (e.g. `ftp://`).
    UnsupportedScheme,
}

#[derive(thiserror::Error, Debug)]
pub enum FetchError {
    #[error("{reason}")]
    InvalidRemoteSpec {
        spec: String,
        kind: RemoteSpecErrorKind,
        reason: String,
    },
    #[error("failed to discover references from '{remote}': {source}")]
    Discovery { remote: String, source: GitError },
    #[error("remote object format '{remote}' does not match local '{local}'")]
    ObjectFormatMismatch { remote: HashKind, local: HashKind },
    #[error("remote branch {branch} not found in upstream {remote}")]
    RemoteBranchNotFound { branch: String, remote: String },
    #[error("failed to fetch objects from '{remote}': {source}")]
    FetchObjects { remote: String, source: io::Error },
    #[error("failed to read fetch stream: {source}")]
    PacketRead { source: io::Error },
    #[error("invalid packet line header '{header}'")]
    InvalidPktHeader { header: String },
    #[error("remote reported an error: {message}")]
    RemoteSideband { message: String },
    #[error(
        "incomplete pack received: the stream ended after {received} bytes before the pack was complete"
    )]
    IncompletePack { received: usize },
    #[error("pack checksum mismatch")]
    ChecksumMismatch,
    #[error("failed to locate objects directory: {source}")]
    ObjectsDirNotFound { source: io::Error },
    #[error("failed to create pack directory '{path}': {source}")]
    PackDirCreate { path: PathBuf, source: io::Error },
    #[error("failed to write pack file '{path}': {source}")]
    PackWrite { path: PathBuf, source: io::Error },
    #[error("failed to build pack index for '{path}': {source}")]
    IndexPack { path: String, source: GitError },
    #[error("failed to update references after fetch: {message}")]
    UpdateRefs { message: String },
    #[error(
        "local Libra remotes do not support --depth yet because they cannot advertise shallow boundaries"
    )]
    UnsupportedShallowLocalLibra,
    #[error("failed to inspect local repository state: {message}")]
    LocalState { message: String },
}

impl From<FetchError> for CliError {
    fn from(error: FetchError) -> Self {
        match &error {
            FetchError::InvalidRemoteSpec { kind, reason, .. } => match kind {
                RemoteSpecErrorKind::MissingLocalRepo => CliError::fatal(reason.clone())
                    .with_stable_code(StableErrorCode::RepoNotFound)
                    .with_hint("check that the remote path exists"),
                RemoteSpecErrorKind::InvalidLocalRepo
                | RemoteSpecErrorKind::MalformedUrl
                | RemoteSpecErrorKind::UnsupportedScheme => CliError::command_usage(reason.clone())
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
                    .with_hint("check the remote URL with 'libra remote get-url <name>'"),
            },
            FetchError::Discovery { source, .. } => {
                map_fetch_discovery_error(error.to_string(), source)
            }
            FetchError::FetchObjects { source, .. } => map_fetch_io_error(
                error.to_string(),
                source,
                StableErrorCode::NetworkUnavailable,
            )
            .with_hint("check network connectivity and retry"),
            FetchError::PacketRead { source } => {
                if is_timeout_io_error(source) {
                    CliError::fatal(error.to_string())
                        .with_stable_code(StableErrorCode::NetworkUnavailable)
                        .with_hint("check network connectivity and retry")
                } else {
                    CliError::fatal(error.to_string())
                        .with_stable_code(StableErrorCode::NetworkProtocol)
                }
            }
            FetchError::RemoteBranchNotFound { .. } => CliError::command_usage(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("verify the remote branch name and try again"),
            FetchError::ObjectFormatMismatch { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid),
            FetchError::IncompletePack { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::NetworkProtocol)
                .with_hint("the connection dropped mid-transfer — retry the fetch"),
            FetchError::InvalidPktHeader { .. }
            | FetchError::RemoteSideband { .. }
            | FetchError::ChecksumMismatch
            | FetchError::IndexPack { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::NetworkProtocol),
            FetchError::ObjectsDirNotFound { .. } => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
            }
            FetchError::PackDirCreate { .. }
            | FetchError::PackWrite { .. }
            | FetchError::UpdateRefs { .. } => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoWriteFailed)
            }
            FetchError::UnsupportedShallowLocalLibra => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint(
                    "omit --depth for local Libra remotes, or fetch from a Git remote that \
                     negotiates shallow boundaries",
                ),
            FetchError::LocalState { .. } => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::RepoCorrupt)
            }
        }
    }
}

fn map_fetch_discovery_error(message: String, source: &GitError) -> CliError {
    match source {
        GitError::UnAuthorized(_) => CliError::fatal(message)
            .with_stable_code(StableErrorCode::AuthPermissionDenied)
            .with_hint("check SSH key / HTTP credentials and repository access rights"),
        GitError::NetworkError(_) => CliError::fatal(message)
            .with_stable_code(StableErrorCode::NetworkUnavailable)
            .with_hint("check network connectivity and retry"),
        GitError::IOError(error) => {
            map_fetch_io_error(message, error, StableErrorCode::NetworkUnavailable)
                .with_hint("check network connectivity and retry")
        }
        _ => CliError::fatal(message).with_stable_code(StableErrorCode::NetworkProtocol),
    }
}

fn map_fetch_io_error(
    message: String,
    error: &std::io::Error,
    default_code: StableErrorCode,
) -> CliError {
    if is_timeout_io_error(error) {
        CliError::fatal(message).with_stable_code(StableErrorCode::NetworkUnavailable)
    } else {
        CliError::fatal(message).with_stable_code(default_code)
    }
}

// `redact_url_credentials` was hoisted to `utils::redact` so `utils`-level
// network clients (D1, git-over-HTTPS) can reuse it. Re-exported here so the
// many existing `fetch::redact_url_credentials` call sites keep working.
pub(crate) use crate::utils::redact::redact_url_credentials;

fn is_timeout_io_error(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::TimedOut {
        return true;
    }
    let lower = error.to_string().to_lowercase();
    lower.contains("timeout") || lower.contains("timed out")
}

pub async fn execute(args: FetchArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting.
///
/// # Side Effects
/// - Reads remote configuration and negotiates refs with one or more remotes.
/// - Downloads pack data and writes received objects into local storage.
/// - Updates remote-tracking refs for fetched branches.
/// - Renders fetch status in the requested output format.
///
/// # Errors
/// Returns [`CliError`] when remote configuration is invalid or missing,
/// authentication/network/pack negotiation fails, object writes fail, or
/// remote-tracking refs cannot be updated.
pub async fn execute_safe(args: FetchArgs, output: &OutputConfig) -> CliResult<()> {
    // `--porcelain` and `--json` are both machine formats; `--json` is a global
    // flag so this exclusion is enforced here (usage error 129), not by clap.
    if args.porcelain && output.is_json() {
        return Err(
            CliError::command_usage("--porcelain and --json are mutually exclusive")
                .with_stable_code(StableErrorCode::CliInvalidArguments),
        );
    }
    let porcelain = args.porcelain;
    let dry_run = args.dry_run;
    let append = args.append;
    let result = run_fetch(args, output).await?;
    // FETCH_HEAD records the fetched refs; `--dry-run` writes nothing.
    if !dry_run {
        write_fetch_head(&result, append).map_err(CliError::from)?;
    }
    if porcelain {
        render_fetch_porcelain(&result, output)
    } else {
        render_fetch_output(&result, output)
    }
}

/// Render Git's `--porcelain` format: one `<flag> <old-oid> <new-oid>
/// <local-ref>` line per ref update, single-space separated, with no human
/// summary columns.
fn render_fetch_porcelain(result: &FetchOutput, output: &OutputConfig) -> CliResult<()> {
    if output.quiet {
        return Ok(());
    }
    let rendered = format_fetch_porcelain(result);
    if rendered.is_empty() {
        return Ok(());
    }
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(writer, "{rendered}")
        .map_err(|error| CliError::io(format!("failed to write fetch output: {error}")))
}

fn format_fetch_porcelain(result: &FetchOutput) -> String {
    let mut lines = Vec::new();
    for remote in &result.remotes {
        for update in &remote.refs_updated {
            let (flag, old_oid) = match &update.old_oid {
                // New ref: space-flag is reserved for fast-forward; new refs use
                // `*` with an all-zero old object id sized to the hash kind.
                None => ('*', "0".repeat(update.new_oid.len())),
                // `+` marks a forced (non-fast-forward / clobbered) update.
                Some(old) if update.forced => ('+', old.clone()),
                Some(old) => (' ', old.clone()),
            };
            lines.push(format!(
                "{flag} {old_oid} {} {}",
                update.new_oid, update.remote_ref
            ));
        }
        // `--prune`/`-p`: a removed ref uses the `-` flag with an all-zero
        // new-oid (the ref no longer exists), structurally isomorphic with the
        // `{flag} {old} {new} {ref}` update lines above. A missing old-oid falls
        // back to the hash-kind-correct zero id (40 hex for SHA-1, 64 for
        // SHA-256); the new-oid zero always matches the old-oid width.
        for entry in &remote.pruned {
            let old_oid = entry
                .old_oid
                .clone()
                .unwrap_or_else(|| ObjectHash::zero_str(get_hash_kind()).to_string());
            let zero = "0".repeat(old_oid.len());
            lines.push(format!("- {old_oid} {zero} {}", entry.remote_ref));
        }
    }
    lines.join("\n")
}

/// Force progress reporting off when `--no-progress` is set (mirroring
/// `git fetch --no-progress`), preserving every other output setting. Returns
/// `Some(modified)` when something changed, or `None` when progress was already
/// off — letting the caller keep borrowing the original `OutputConfig`. Shared
/// with `pull --no-progress`, which forwards the same suppression to its fetch.
pub(crate) fn apply_no_progress(output: &OutputConfig, no_progress: bool) -> Option<OutputConfig> {
    if no_progress && !matches!(output.progress, ProgressMode::None) {
        let mut suppressed = output.clone();
        suppressed.progress = ProgressMode::None;
        suppressed.progress_preference = ProgressPreference::None;
        Some(suppressed)
    } else {
        None
    }
}

async fn run_fetch(args: FetchArgs, output: &OutputConfig) -> CliResult<FetchOutput> {
    tracing::debug!("`fetch` args: {:?}", args);

    let FetchArgs {
        repository,
        refspec,
        all,
        depth,
        dry_run,
        append: _,
        verbose,
        porcelain: _,
        force,
        tags,
        no_tags,
        no_auto_gc: _,
        no_progress,
        prune,
        no_prune: _,
        notes,
    } = args;

    // `--no-progress` forces progress reporting off (the "Receiving objects"
    // spinner and any NDJSON progress events), mirroring `git fetch
    // --no-progress`. All other output settings are preserved.
    let suppressed_output = apply_no_progress(output, no_progress);
    let output = suppressed_output.as_ref().unwrap_or(output);

    // Resolve the CLI tag intent: `--tags` -> All, `--no-tags` -> NoTags, neither
    // -> None (let each remote fall back to `remote.<name>.tagOpt` then the Git
    // default auto-follow). `overrides_with` guarantees the two flags can't both
    // be set.
    let tag_cli = if tags {
        Some(TagFetchMode::All)
    } else if no_tags {
        Some(TagFetchMode::NoTags)
    } else {
        None
    };

    if all {
        let remotes = ConfigKv::all_remote_configs().await.map_err(|error| {
            CliError::fatal(format!("failed to read remote configuration: {error}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;

        let mut results = Vec::with_capacity(remotes.len());
        for remote in remotes {
            if verbose {
                eprintln!(
                    "Fetching {} from {}",
                    remote.name,
                    redact_url_credentials(&remote.url)
                );
            }
            results.push(
                fetch_repository_with_result(
                    remote, None, false, depth, dry_run, tag_cli, force, prune, notes, output,
                )
                .await
                .map_err(CliError::from)?,
            );
        }

        return Ok(FetchOutput {
            all: true,
            requested_remote: None,
            refspec: None,
            remotes: results,
        });
    }

    let remote = match repository {
        Some(remote) => remote,
        None => match ConfigKv::get_current_remote().await {
            Ok(Some(remote)) => remote,
            Ok(None) => {
                return Err(
                    CliError::fatal("no configured remote for the current branch")
                        .with_stable_code(StableErrorCode::RepoStateInvalid)
                        .with_hint("use 'libra remote add <name> <url>' to configure a remote"),
                );
            }
            Err(_) => {
                return Err(CliError::fatal("HEAD is detached")
                    .with_stable_code(StableErrorCode::RepoStateInvalid)
                    .with_hint("switch to a branch before fetching its upstream"));
            }
        },
    };

    let remote_config = ConfigKv::remote_config(&remote)
        .await
        .map_err(|error| {
            CliError::fatal(format!("failed to read remote configuration: {error}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?
        .ok_or_else(|| {
            CliError::fatal(format!("remote '{remote}' not found"))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("use 'libra remote -v' to inspect configured remotes")
        })?;

    if verbose {
        eprintln!(
            "Fetching {} from {}",
            remote_config.name,
            redact_url_credentials(&remote_config.url)
        );
    }

    let result = fetch_repository_with_result(
        remote_config,
        refspec.clone(),
        false,
        depth,
        dry_run,
        tag_cli,
        force,
        prune,
        notes,
        output,
    )
    .await
    .map_err(CliError::from)?;

    Ok(FetchOutput {
        all: false,
        requested_remote: Some(remote),
        refspec,
        remotes: vec![result],
    })
}

fn render_fetch_output(result: &FetchOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("fetch", result, output);
    }

    if output.quiet {
        return Ok(());
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();

    if result.remotes.is_empty() {
        writeln!(writer, "No remotes configured")
            .map_err(|error| CliError::io(format!("failed to write fetch output: {error}")))?;
        return Ok(());
    }

    for (index, remote) in result.remotes.iter().enumerate() {
        if index > 0 {
            writeln!(writer)
                .map_err(|error| CliError::io(format!("failed to write fetch output: {error}")))?;
        }

        // `remote.url` is already credential-redacted at construction time in
        // `fetch_repository_with_result`, so no additional redaction needed here.
        writeln!(writer, "From {}", remote.url)
            .map_err(|error| CliError::io(format!("failed to write fetch output: {error}")))?;

        if remote.refs_updated.is_empty() && remote.pruned.is_empty() {
            writeln!(writer, "Already up to date with '{}'", remote.remote)
                .map_err(|error| CliError::io(format!("failed to write fetch output: {error}")))?;
            continue;
        }

        for update in &remote.refs_updated {
            let is_tag = update.remote_ref.starts_with("refs/tags/");
            let ref_name = update
                .remote_ref
                .strip_prefix("refs/remotes/")
                .unwrap_or(&update.remote_ref);
            match &update.old_oid {
                None if is_tag => {
                    writeln!(writer, " * [new tag]         {}", ref_name).map_err(|error| {
                        CliError::io(format!("failed to write fetch output: {error}"))
                    })?
                }
                None => writeln!(writer, " * [new ref]         {}", ref_name).map_err(|error| {
                    CliError::io(format!("failed to write fetch output: {error}"))
                })?,
                Some(old_oid) => {
                    let old_short = &old_oid[..7.min(old_oid.len())];
                    let new_short = &update.new_oid[..7.min(update.new_oid.len())];
                    if update.forced {
                        writeln!(
                            writer,
                            " + {}...{}  {} (forced update)",
                            old_short, new_short, ref_name
                        )
                        .map_err(|error| {
                            CliError::io(format!("failed to write fetch output: {error}"))
                        })?;
                    } else {
                        writeln!(writer, "   {}..{}  {}", old_short, new_short, ref_name).map_err(
                            |error| CliError::io(format!("failed to write fetch output: {error}")),
                        )?;
                    }
                }
            }
        }

        // `--prune`/`-p`: report each removed stale remote-tracking ref, in the
        // `<remote>/<branch>` display form used for the other ref lines.
        for entry in &remote.pruned {
            writeln!(
                writer,
                " - [deleted]         (none)     -> {}",
                entry.branch
            )
            .map_err(|error| CliError::io(format!("failed to write fetch output: {error}")))?;
        }

        writeln!(writer, " {} objects fetched", remote.objects_fetched)
            .map_err(|error| CliError::io(format!("failed to write fetch output: {error}")))?;
    }

    Ok(())
}

pub(crate) async fn discover_remote(
    remote_spec: &str,
) -> Result<(RemoteClient, DiscoveryResult), FetchError> {
    discover_remote_with_name(remote_spec, None).await
}

/// Like [`discover_remote`] but accepts an optional logical remote name
/// so that vault-backed SSH keys (`vault.ssh.<remote>.privkey`) can be
/// resolved during transport setup.
pub(crate) async fn discover_remote_with_name(
    remote_spec: &str,
    remote_name: Option<&str>,
) -> Result<(RemoteClient, DiscoveryResult), FetchError> {
    let remote_client = RemoteClient::from_spec_with_remote(remote_spec, remote_name)
        .and_then(|client| client.with_resolved_fetch_timeouts(remote_name))
        .map_err(|message| {
            let (kind, reason) = classify_remote_spec_error(remote_spec, &message);
            FetchError::InvalidRemoteSpec {
                spec: remote_spec.to_string(),
                kind,
                reason,
            }
        })?;
    let discovery = remote_client
        .discovery_reference(UploadPack)
        .await
        .map_err(|source| FetchError::Discovery {
            remote: remote_spec.to_string(),
            source,
        })?;
    Ok((remote_client, discovery))
}

/// Classify a remote-spec construction failure into a typed kind and a
/// human-readable reason string.
fn classify_remote_spec_error(remote_spec: &str, message: &str) -> (RemoteSpecErrorKind, String) {
    if message.starts_with("invalid local repository") {
        let display = if remote_spec == "/" {
            "/".to_string()
        } else {
            remote_spec.trim_end_matches('/').to_string()
        };
        let lower = message.to_ascii_lowercase();
        if lower.contains("no such file or directory")
            || lower.contains("does not exist")
            || lower.contains("not found")
        {
            return (
                RemoteSpecErrorKind::MissingLocalRepo,
                format!("repository '{}' does not exist", display),
            );
        }
        return (
            RemoteSpecErrorKind::InvalidLocalRepo,
            format!("'{}' does not appear to be a libra repository", display),
        );
    }
    let lower = message.to_ascii_lowercase();
    if lower.contains("unsupported") && lower.contains("scheme") {
        return (RemoteSpecErrorKind::UnsupportedScheme, message.to_string());
    }
    // Default to MalformedUrl for other spec errors (bad syntax, etc.)
    (RemoteSpecErrorKind::MalformedUrl, message.to_string())
}

pub(crate) fn normalize_branch_ref(branch: &str) -> String {
    if branch.starts_with("refs/") {
        branch.to_string()
    } else {
        format!("refs/heads/{branch}")
    }
}

pub(crate) fn remote_has_branch(refs: &[DiscRef], branch: &str) -> bool {
    let normalized = normalize_branch_ref(branch);
    refs.iter().any(|reference| reference._ref == normalized)
}

pub(crate) fn normalize_remote_url(remote_input: &str, remote_client: &RemoteClient) -> String {
    match remote_client {
        RemoteClient::Http(_) | RemoteClient::Git(_) | RemoteClient::Ssh(_) => {
            remote_input.to_string()
        }
        RemoteClient::Local(client) => client.repo_path().to_string_lossy().to_string(),
    }
}

/// Fetch from remote repository
/// - `branch` is optional, if `None`, fetch all branches
/// - `single_branch` is bool, if `true`, fetch only the specified branch
/// - `depth` is optional, if `Some(n)`, create a shallow clone with history truncated to n commits
pub async fn fetch_repository(
    remote_config: RemoteConfig,
    branch: Option<String>,
    single_branch: bool,
    depth: Option<usize>,
) {
    if let Err(err) = fetch_repository_safe(
        remote_config,
        branch,
        single_branch,
        depth,
        None,
        &OutputConfig::default(),
    )
    .await
    {
        CliError::from(err).print_stderr();
    }
}

pub async fn fetch_repository_safe(
    remote_config: RemoteConfig,
    branch: Option<String>,
    single_branch: bool,
    depth: Option<usize>,
    tag_cli: Option<TagFetchMode>,
    output: &OutputConfig,
) -> Result<(), FetchError> {
    fetch_repository_with_result(
        remote_config,
        branch,
        single_branch,
        depth,
        false,
        tag_cli,
        false,
        false,
        false,
        output,
    )
    .await
    .map(|_| ())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn fetch_repository_with_result(
    remote_config: RemoteConfig,
    branch: Option<String>,
    single_branch: bool,
    depth: Option<usize>,
    dry_run: bool,
    tag_cli: Option<TagFetchMode>,
    force: bool,
    prune: bool,
    notes: bool,
    output: &OutputConfig,
) -> Result<FetchRepositoryResult, FetchError> {
    let (remote_client, discovery) =
        discover_remote_with_name(&remote_config.url, Some(&remote_config.name)).await?;
    // Redact credentials from the URL before storing it in the result to
    // prevent secret leakage in both human and JSON output.
    let normalized_url =
        redact_url_credentials(&normalize_remote_url(&remote_config.url, &remote_client));
    if depth.is_some()
        && matches!(&remote_client, RemoteClient::Local(client) if client.is_libra_source())
    {
        return Err(FetchError::UnsupportedShallowLocalLibra);
    }
    let local_kind = get_hash_kind();
    if discovery.hash_kind != local_kind {
        return Err(FetchError::ObjectFormatMismatch {
            remote: discovery.hash_kind,
            local: local_kind,
        });
    }
    set_wire_hash_kind(discovery.hash_kind);

    if let Some(branch_name) = &branch
        && !remote_has_branch(&discovery.refs, branch_name)
    {
        return Err(FetchError::RemoteBranchNotFound {
            branch: branch_name.clone(),
            remote: remote_config.name.clone(),
        });
    }

    let mut refs = discovery.refs.clone();
    if refs.is_empty() {
        tracing::debug!("fetch skipped because remote has no refs");
        // Conservatively skip pruning when the remote advertises no refs at all
        // (a transient/broken advertisement) so a single empty response cannot
        // wipe every remote-tracking ref.
        return Ok(FetchRepositoryResult {
            remote: remote_config.name,
            url: normalized_url,
            refs_updated: Vec::new(),
            objects_fetched: 0,
            bytes_received: 0,
            pruned: Vec::new(),
        });
    }

    let remote_head = refs
        .iter()
        .find(|reference| reference._ref == HEAD)
        .cloned();
    let ref_heads = refs
        .iter()
        .filter(|reference| reference._ref.starts_with("refs/heads/"))
        .cloned()
        .collect::<Vec<_>>();

    // Resolve tag handling for this remote (CLI > `remote.<name>.tagOpt` > auto).
    let tag_mode = resolve_tag_mode(&remote_config.name, tag_cli).await;
    // Every advertised tag ref (excluding peeled `^{}` lines), captured before
    // the want-filter drops them. Used to persist `--tags` / auto-followed tags.
    let discovered_tags: Vec<DiscRef> = discovery
        .refs
        .iter()
        .filter(|r| r._ref.starts_with("refs/tags/") && !r._ref.ends_with("^{}"))
        .cloned()
        .collect();

    // Only request refs we will actually persist. `update_references` saves
    // `refs/heads/*` and `refs/mr/*`; with `--tags` (`All`) we also explicitly
    // `want` `refs/tags/*`. Asking for anything else (HEAD symref, `refs/pull/*`)
    // makes the server include unreachable objects that the next fetch's `have`
    // cannot cover, forcing the same pack to be re-downloaded every time. Tags
    // are safe to keep because `current_have_safe` seeds `have` from local
    // `refs/tags/*` (peeling annotated tags). Auto-followed tags are NOT wanted
    // here — they arrive via the `include-tag` capability and are persisted
    // post-fetch only when their object/target is present.
    refs.retain(|reference| {
        reference._ref.starts_with("refs/heads/")
            || reference._ref.starts_with("refs/mr/")
            || (tag_mode == TagFetchMode::All
                && reference._ref.starts_with("refs/tags/")
                && !reference._ref.ends_with("^{}"))
    });

    if let Some(branch_name) = &branch
        && single_branch
    {
        let normalized = normalize_branch_ref(branch_name);
        refs.retain(|reference| reference._ref == normalized);
    }

    // `--dry-run`: compute the would-be remote-tracking ref updates from the
    // discovered refs and return before downloading any pack or writing
    // anything (no `.pack`/`.idx`, no shallow update, no ref/reflog writes, no
    // FETCH_HEAD).
    if dry_run {
        let refs_updated = compute_fetch_ref_preview(&remote_config, &refs).await?;
        // `--dry-run --prune`: report the stale refs that would be removed, but
        // write nothing.
        let pruned = if prune {
            prune_stale_remote_refs(
                &remote_config.name,
                &remote_advertised_branch_names(&discovery.refs),
                true,
            )
            .await?
        } else {
            Vec::new()
        };
        return Ok(FetchRepositoryResult {
            remote: remote_config.name,
            url: normalized_url,
            refs_updated,
            objects_fetched: 0,
            bytes_received: 0,
            pruned,
        });
    }

    let mut want = refs
        .iter()
        .map(|reference| reference._hash.clone())
        .collect::<Vec<_>>();
    want.sort();
    want.dedup();
    let have = current_have_safe().await?;
    let shallow_boundaries = read_shallow_boundaries()?;
    let shallow = shallow_boundaries.iter().cloned().collect::<Vec<_>>();
    let mut result_stream = remote_client
        .fetch_objects(&have, &want, &shallow, depth)
        .await
        .map_err(|source| FetchError::FetchObjects {
            remote: remote_config.url.clone(),
            source,
        })?;

    let task = format!("fetch {}", remote_config.name);
    let fetch_data = read_fetch_stream(&mut result_stream, output, &task).await?;
    let objects_fetched = pack_object_count(&fetch_data.pack_data);
    let bytes_received = fetch_data.pack_data.len();
    let pack_file = write_pack_and_index(&fetch_data.pack_data)?;
    if let Some(pack_file) = pack_file {
        let index_version = match get_hash_kind() {
            HashKind::Sha1 => None,
            HashKind::Sha256 => Some(2),
        };
        match index_version {
            Some(2) => index_pack::build_index_v2(&pack_file, &pack_file.replace(".pack", ".idx"))
                .map_err(|source| FetchError::IndexPack {
                    path: pack_file.clone(),
                    source,
                })?,
            _ => index_pack::build_index_v1(&pack_file, &pack_file.replace(".pack", ".idx"))
                .map_err(|source| FetchError::IndexPack {
                    path: pack_file.clone(),
                    source,
                })?,
        }
    }
    apply_shallow_updates(&fetch_data.shallow, &fetch_data.unshallow)?;

    let mut refs_updated = update_references(
        &remote_config,
        &refs,
        &ref_heads,
        remote_head,
        branch,
        discovery.capabilities.clone(),
    )
    .await?;

    // Persist tags per the resolved mode. `All`: every advertised tag (already
    // in `want`). `AutoFollow`: tags whose object/target is now present locally
    // — annotated tag objects arrive via the `include-tag` capability when their
    // target was fetched; lightweight tags need only their commit. `NoTags`: none.
    let tags_to_persist: Vec<DiscRef> = match tag_mode {
        TagFetchMode::NoTags => Vec::new(),
        TagFetchMode::All => discovered_tags,
        TagFetchMode::AutoFollow => {
            let storage = util::objects_storage();
            discovered_tags
                .into_iter()
                .filter(|tag| {
                    ObjectHash::from_str(&tag._hash)
                        .map(|oid| storage.exist(&oid))
                        .unwrap_or(false)
                })
                .collect()
        }
    };
    refs_updated.extend(persist_fetched_tags(&tags_to_persist, force).await?);

    // Dependency-graph notes (`refs/notes/deps`, lore.md 3.2). When `--notes` (or
    // the persisted `remote.<name>.fetchNotesDeps` opt-in) is set, pull the
    // source's file-dependency graph over a dedicated side-channel: a Libra deps
    // note is a loose blob + a SQLite row, NOT a commit-reachable object, so it
    // cannot ride the pack. v1 travels notes only from a local Libra source; a
    // network or foreign-Git remote emits an honest deferred warning (D17). This
    // is per-note fault-tolerant and never aborts a fetch whose refs are already
    // updated.
    let notes_enabled = notes || remote_fetch_notes_deps(&remote_config.name).await;
    if notes_enabled {
        for warning in fetch_deps_notes(&remote_client).await {
            eprintln!("warning: {warning}");
        }
    }

    // `--prune`/`-p`: after the fetch has updated tracking refs, delete any
    // `refs/remotes/<name>/*` the remote no longer advertises (transactionally,
    // with an audit reflog entry). Only stale tracking refs for *this* remote
    // are touched.
    let pruned = if prune {
        prune_stale_remote_refs(
            &remote_config.name,
            &remote_advertised_branch_names(&discovery.refs),
            false,
        )
        .await?
    } else {
        Vec::new()
    };

    Ok(FetchRepositoryResult {
        remote: remote_config.name,
        url: normalized_url,
        refs_updated,
        objects_fetched,
        bytes_received,
        pruned,
    })
}

/// Effective `--notes` opt-in for a remote: the CLI flag OR the persisted
/// `remote.<name>.fetchNotesDeps=true` config (written by `clone --deps-of` so
/// later pulls keep the dependency graph fresh). Best-effort — a missing/broken
/// config resolves to `false`, never an error.
async fn remote_fetch_notes_deps(remote_name: &str) -> bool {
    match ConfigKv::get_best_effort(&format!("remote.{remote_name}.fetchNotesDeps")).await {
        Ok(Some(entry)) => matches!(entry.value.trim(), "true" | "1" | "yes" | "on"),
        _ => false,
    }
}

/// Import the file-dependency graph (`refs/notes/deps`, lore.md 3.2) from
/// `remote_client` into the current repo. Returns non-fatal warnings (a deferred
/// remote, or a skipped malformed / absent-commit note). NEVER aborts the fetch:
/// the graph is best-effort metadata layered on top of an already-completed pack
/// + ref update.
async fn fetch_deps_notes(remote_client: &RemoteClient) -> Vec<String> {
    match remote_client {
        RemoteClient::Local(client) => {
            let (entries, mut warnings) = match client.export_deps_notes().await {
                Ok(pair) => pair,
                Err(e) => (
                    Vec::new(),
                    vec![format!(
                        "could not read the dependency graph from the source: {e}"
                    )],
                ),
            };
            let outcome = crate::internal::deps::DependencyStore::import_notes(&entries).await;
            if outcome.imported > 0 {
                tracing::debug!(
                    "imported {} dependency note(s) from the remote",
                    outcome.imported
                );
            }
            warnings.extend(outcome.warnings);
            warnings
        }
        _ => vec![
            "dependency-graph (refs/notes/deps) travel over network remotes is not supported \
             yet; the graph was not fetched (see _compatibility.md D17)"
                .to_string(),
        ],
    }
}

#[derive(Default)]
struct FetchStreamData {
    pack_data: Vec<u8>,
    shallow: Vec<String>,
    unshallow: Vec<String>,
}

/// Tracks packfile boundaries so fetch can finish once the pack checksum is
/// present, even if the SSH transport stays open after `git-upload-pack` is done.
#[derive(Default)]
struct PackCompletionTracker {
    object_count: Option<usize>,
    objects_seen: usize,
    offset: usize,
    current_object: Option<PackObjectInflate>,
    complete: bool,
}

struct PackObjectInflate {
    start: usize,
    inflater: flate2::Decompress,
}

impl PackCompletionTracker {
    fn observe(&mut self, pack_data: &[u8]) -> bool {
        if self.complete {
            return true;
        }

        if self.object_count.is_none() && !self.read_header(pack_data) {
            return false;
        }

        let Some(object_count) = self.object_count else {
            return false;
        };

        while self.objects_seen < object_count {
            if !self.advance_object(pack_data) {
                return false;
            }
        }

        self.complete = self.has_valid_trailing_checksum(pack_data);
        self.complete
    }

    fn read_header(&mut self, pack_data: &[u8]) -> bool {
        if pack_data.len() < 12 || &pack_data[..4] != b"PACK" {
            return false;
        }
        let Some(version) = read_be_u32(pack_data, 4) else {
            return false;
        };
        if version != 2 && version != 3 {
            return false;
        }
        let Some(object_count) = read_be_u32(pack_data, 8) else {
            return false;
        };
        self.object_count = Some(object_count as usize);
        self.offset = 12;
        true
    }

    fn advance_object(&mut self, pack_data: &[u8]) -> bool {
        if self.current_object.is_none() {
            let Some(data_offset) =
                parse_pack_entry_data_offset(pack_data, self.offset, get_hash_kind().size())
            else {
                return false;
            };
            self.current_object = Some(PackObjectInflate {
                start: data_offset,
                inflater: flate2::Decompress::new(true),
            });
        }

        let complete_offset = {
            let Some(current) = self.current_object.as_mut() else {
                return false;
            };
            let mut output = [0_u8; 8192];
            loop {
                let consumed = current.inflater.total_in() as usize;
                let Some(input_offset) = current.start.checked_add(consumed) else {
                    return false;
                };
                let Some(input) = pack_data.get(input_offset..) else {
                    return false;
                };
                if input.is_empty() {
                    return false;
                }

                let before_in = current.inflater.total_in();
                let before_out = current.inflater.total_out();
                let status = match current.inflater.decompress(
                    input,
                    &mut output,
                    flate2::FlushDecompress::None,
                ) {
                    Ok(status) => status,
                    Err(_) => return false,
                };
                if matches!(status, flate2::Status::StreamEnd) {
                    break current
                        .start
                        .checked_add(current.inflater.total_in() as usize);
                }
                if before_in == current.inflater.total_in()
                    && before_out == current.inflater.total_out()
                {
                    return false;
                }
            }
        };

        let Some(complete_offset) = complete_offset else {
            return false;
        };
        self.offset = complete_offset;
        self.current_object = None;
        self.objects_seen += 1;
        true
    }

    fn has_valid_trailing_checksum(&self, pack_data: &[u8]) -> bool {
        let hash_len = get_hash_kind().size();
        let Some(end) = self.offset.checked_add(hash_len) else {
            return false;
        };
        if pack_data.len() != end {
            return false;
        }
        let expected = ObjectHash::new(&pack_data[..self.offset]);
        ObjectHash::from_bytes(&pack_data[self.offset..end]).is_ok_and(|actual| actual == expected)
    }
}

fn read_be_u32(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn parse_pack_entry_data_offset(
    pack_data: &[u8],
    mut offset: usize,
    hash_len: usize,
) -> Option<usize> {
    let first = *pack_data.get(offset)?;
    offset += 1;
    let object_type = (first >> 4) & 0b111;
    let mut byte = first;
    while byte & 0x80 != 0 {
        byte = *pack_data.get(offset)?;
        offset += 1;
    }

    match object_type {
        1..=4 => Some(offset),
        6 => {
            byte = *pack_data.get(offset)?;
            offset += 1;
            while byte & 0x80 != 0 {
                byte = *pack_data.get(offset)?;
                offset += 1;
            }
            Some(offset)
        }
        7 => offset
            .checked_add(hash_len)
            .filter(|end| *end <= pack_data.len()),
        _ => None,
    }
}

async fn read_fetch_stream(
    result_stream: &mut FetchStream,
    output: &OutputConfig,
    task: &str,
) -> Result<FetchStreamData, FetchError> {
    let mut reader = StreamReader::new(result_stream);
    let mut data_out = FetchStreamData::default();
    let mut pack_completion = PackCompletionTracker::default();
    let mut reach_pack = false;
    let mut saw_shallow_response = false;
    let render_progress = matches!(output.progress, ProgressMode::Text);
    let json_progress = matches!(output.progress, ProgressMode::Json);
    let bar = render_progress.then(ProgressBar::new_spinner);
    let progress = json_progress.then(|| ProgressReporter::new(task, None, output));
    let mut remote_progress = RemoteProgressBuffer::default();
    let time = Instant::now();

    loop {
        let (len, data) = match read_pkt_line(&mut reader).await {
            Ok(packet) => packet,
            Err(source) if source.kind() == io::ErrorKind::UnexpectedEof && reach_pack => break,
            Err(source) => return Err(FetchError::PacketRead { source }),
        };
        if len == 0 {
            if !reach_pack && saw_shallow_response {
                saw_shallow_response = false;
                continue;
            }
            break;
        }
        if !reach_pack {
            if let Some(oid) = parse_shallow_packet(&data, b"shallow ") {
                data_out.shallow.push(oid);
                saw_shallow_response = true;
                continue;
            }
            if let Some(oid) = parse_shallow_packet(&data, b"unshallow ") {
                data_out.unshallow.push(oid);
                saw_shallow_response = true;
                continue;
            }
            if data.starts_with(b"PACK") {
                reach_pack = true;
                data_out.pack_data.extend(&data);
                if let Some(progress) = &progress {
                    progress.tick(data_out.pack_data.len() as u64);
                }
                if pack_completion.observe(&data_out.pack_data) {
                    break;
                }
                continue;
            }
        }
        if data.len() >= 5 && data[0] == 1 && &data[1..5] == b"PACK" {
            reach_pack = true;
        }

        if reach_pack {
            if let Some((&code, payload)) = data.split_first() {
                match code {
                    1 => {
                        let bytes_per_sec =
                            data_out.pack_data.len() as f64 / time.elapsed().as_secs_f64();
                        let total = util::auto_unit_bytes(data_out.pack_data.len() as u64);
                        let bps = util::auto_unit_bytes(bytes_per_sec as u64);
                        if let Some(bar) = &bar {
                            bar.set_message(format!("Receiving objects: {total:.2} | {bps:.2}/s"));
                            bar.tick();
                        }
                        data_out.pack_data.extend(payload);
                        if let Some(progress) = &progress {
                            progress.tick(data_out.pack_data.len() as u64);
                        }
                        if pack_completion.observe(&data_out.pack_data) {
                            break;
                        }
                    }
                    2 => handle_remote_progress(
                        payload,
                        render_progress,
                        bar.as_ref(),
                        &mut remote_progress,
                    ),
                    3 => {
                        flush_remote_progress(render_progress, bar.as_ref(), &mut remote_progress);
                        if let Some(bar) = &bar {
                            bar.finish_and_clear();
                        }
                        return Err(FetchError::RemoteSideband {
                            message: clean_sideband_message(payload),
                        });
                    }
                    _ => {
                        tracing::debug!("ignoring unknown side-band code {code}");
                    }
                }
            }
        } else if data != b"NAK\n"
            && !data.starts_with(b"ACK ")
            && !data.starts_with(b"shallow ")
            && !data.starts_with(b"unshallow ")
            && let Some((&code, payload)) = data.split_first()
        {
            match code {
                2 => handle_remote_progress(
                    payload,
                    render_progress,
                    bar.as_ref(),
                    &mut remote_progress,
                ),
                3 => {
                    flush_remote_progress(render_progress, bar.as_ref(), &mut remote_progress);
                    if let Some(bar) = &bar {
                        bar.finish_and_clear();
                    }
                    return Err(FetchError::RemoteSideband {
                        message: clean_sideband_message(payload),
                    });
                }
                _ => {
                    tracing::debug!(
                        "ignoring pre-pack frame: {:?}",
                        String::from_utf8_lossy(&data)
                    );
                }
            }
        }
    }
    flush_remote_progress(render_progress, bar.as_ref(), &mut remote_progress);
    if let Some(bar) = &bar {
        bar.finish_and_clear();
    }
    if let Some(progress) = &progress {
        progress.finish();
    }

    // The pack started but the stream ended before it was complete (a truncated
    // or half-delivered pack). Fail loudly with a clear protocol error rather
    // than handing a partial pack downstream — references are never updated on
    // this path, so the already-received objects are simply left for `gc`.
    if reach_pack && !pack_completion.complete {
        tracing::warn!(
            "incomplete pack received: {} bytes before the stream ended; \
             discarding — references were not updated",
            data_out.pack_data.len()
        );
        return Err(FetchError::IncompletePack {
            received: data_out.pack_data.len(),
        });
    }

    Ok(data_out)
}

/// Strip a leading `ERR ` / `FATAL ` marker from a side-band channel-3 message so
/// the surfaced fetch error reads as the remote's own text rather than repeating
/// the wire marker (`remote reported an error: ERR access denied` → `… access
/// denied`).
fn clean_sideband_message(payload: &[u8]) -> String {
    let text = String::from_utf8_lossy(payload);
    let trimmed = text.trim();
    for marker in ["ERR ", "FATAL ", "ERR: ", "FATAL: "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return rest.trim().to_string();
        }
    }
    trimmed.to_string()
}

fn parse_shallow_packet(data: &[u8], prefix: &[u8]) -> Option<String> {
    let raw = data.strip_prefix(prefix)?;
    let text = std::str::from_utf8(raw).ok()?.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Line-buffers raw sideband progress bytes so the indicatif spinner and the
/// remote's `\r`-overwriting progress text do not stomp on each other.
///
/// Git's smart protocol delivers human-readable progress on side-band 2 in
/// arbitrarily small chunks that may split mid-word. The remote uses `\r` for
/// in-place updates (e.g. `Counting objects:  5%\rCounting objects: 10%\r…`)
/// and `\n` to commit a line (e.g. `Counting objects: 100% (38/38), done.\n`).
/// Forwarding raw bytes straight to `eprint!` while the local spinner is also
/// being redrawn produces interleaved fragments separated by spinner ticks.
#[derive(Default)]
struct RemoteProgressBuffer {
    buf: String,
}

impl RemoteProgressBuffer {
    /// Append `payload` and dispatch any complete lines.
    ///
    /// - `\n`-terminated (and `\r\n`-terminated) lines are emitted to
    ///   `on_permanent` so the caller can promote them to a log line above
    ///   the bar.
    /// - `\r`-terminated lines are emitted to `on_transient`, which typically
    ///   maps to `bar.set_message` so the latest progress replaces the prior.
    /// - Any trailing partial content stays in the buffer for the next call.
    fn push<P, T>(&mut self, payload: &[u8], mut on_permanent: P, mut on_transient: T)
    where
        P: FnMut(&str),
        T: FnMut(&str),
    {
        if payload.is_empty() {
            return;
        }
        self.buf.push_str(&String::from_utf8_lossy(payload));
        while let Some(pos) = self.buf.find(['\r', '\n']) {
            // ASCII terminators are always at char boundaries, so split_off is safe.
            let terminator = self.buf.as_bytes()[pos];
            let line: String = self.buf.drain(..pos).collect();
            self.buf.drain(..1);

            // Treat CRLF as a single newline so we don't emit an extra empty transient.
            let is_permanent =
                terminator == b'\n' || (terminator == b'\r' && self.buf.starts_with('\n'));
            if terminator == b'\r' && self.buf.starts_with('\n') {
                self.buf.drain(..1);
            }
            if is_permanent {
                on_permanent(&line);
            } else {
                on_transient(&line);
            }
        }
    }

    /// Emit any unterminated trailing bytes as a permanent line.
    ///
    /// Called once the sideband stream has ended so we never silently drop
    /// the last fragment when the remote closed without a final newline.
    fn flush_remaining<P>(&mut self, mut on_permanent: P)
    where
        P: FnMut(&str),
    {
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            on_permanent(&line);
        }
    }
}

fn handle_remote_progress(
    payload: &[u8],
    render_progress: bool,
    bar: Option<&ProgressBar>,
    buffer: &mut RemoteProgressBuffer,
) {
    if !render_progress {
        return;
    }
    buffer.push(
        payload,
        |line| emit_permanent_progress_line(line, bar),
        |line| emit_transient_progress_line(line, bar),
    );
}

fn flush_remote_progress(
    render_progress: bool,
    bar: Option<&ProgressBar>,
    buffer: &mut RemoteProgressBuffer,
) {
    if !render_progress {
        return;
    }
    buffer.flush_remaining(|line| emit_permanent_progress_line(line, bar));
}

fn emit_permanent_progress_line(line: &str, bar: Option<&ProgressBar>) {
    if let Some(bar) = bar {
        // `println` clears the bar, prints the line, then redraws the bar
        // below — the canonical way to interleave logs with an indicatif spinner.
        bar.println(line);
    } else {
        let mut stderr = io::stderr().lock();
        let _ = writeln!(stderr, "{line}");
    }
}

fn emit_transient_progress_line(line: &str, bar: Option<&ProgressBar>) {
    if let Some(bar) = bar {
        bar.set_message(line.to_owned());
        bar.tick();
    } else {
        let mut stderr = io::stderr().lock();
        let _ = write!(stderr, "\r{line}");
        let _ = stderr.flush();
    }
}

fn pack_object_count(pack_data: &[u8]) -> usize {
    if pack_data.len() < 12 || &pack_data[..4] != b"PACK" {
        return 0;
    }
    let mut count = [0u8; 4];
    count.copy_from_slice(&pack_data[8..12]);
    u32::from_be_bytes(count) as usize
}

fn write_pack_and_index(pack_data: &[u8]) -> Result<Option<String>, FetchError> {
    let hash_len = get_hash_kind().size();
    if pack_data.len() < hash_len {
        tracing::debug!("No pack data returned from remote");
        return Ok(None);
    }

    let payload_len = pack_data.len() - hash_len;
    let hash = ObjectHash::new(&pack_data[..payload_len]);
    let checksum = ObjectHash::from_bytes(&pack_data[payload_len..])
        .map_err(|_| FetchError::ChecksumMismatch)?;
    if hash != checksum {
        return Err(FetchError::ChecksumMismatch);
    }

    if pack_data.len() <= 12 + hash_len {
        tracing::debug!("Empty pack file");
        return Ok(None);
    }

    let pack_dir = path::try_objects()
        .map_err(|source| FetchError::ObjectsDirNotFound { source })?
        .join("pack");
    fs::create_dir_all(&pack_dir).map_err(|source| FetchError::PackDirCreate {
        path: pack_dir.clone(),
        source,
    })?;

    let checksum = checksum.to_string();
    let pack_file = pack_dir.join(format!("pack-{checksum}.pack"));
    let mut file = fs::File::create(&pack_file).map_err(|source| FetchError::PackWrite {
        path: pack_file.clone(),
        source,
    })?;
    file.write_all(pack_data)
        .map_err(|source| FetchError::PackWrite {
            path: pack_file.clone(),
            source,
        })?;

    Ok(Some(pack_file.to_string_lossy().into_owned()))
}

fn shallow_file_path() -> Result<PathBuf, FetchError> {
    util::try_get_storage_path(None)
        .map(|storage| storage.join("shallow"))
        .map_err(|source| FetchError::LocalState {
            message: format!("failed to locate repository storage for shallow metadata: {source}"),
        })
}

fn read_shallow_boundaries() -> Result<BTreeSet<String>, FetchError> {
    let path = shallow_file_path()?;
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(source) => {
            return Err(FetchError::LocalState {
                message: format!(
                    "failed to read shallow metadata '{}': {source}",
                    path.display()
                ),
            });
        }
    };

    let mut boundaries = BTreeSet::new();
    for (line_no, line) in content.lines().enumerate() {
        let oid = line.trim();
        if oid.is_empty() {
            continue;
        }
        ObjectHash::from_str(oid).map_err(|source| FetchError::LocalState {
            message: format!(
                "invalid shallow metadata entry at '{}:{}': {source}",
                path.display(),
                line_no + 1
            ),
        })?;
        boundaries.insert(oid.to_string());
    }
    Ok(boundaries)
}

fn write_shallow_boundaries(boundaries: &BTreeSet<String>) -> Result<(), FetchError> {
    let path = shallow_file_path()?;
    if boundaries.is_empty() {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(FetchError::LocalState {
                    message: format!(
                        "failed to remove shallow metadata '{}': {source}",
                        path.display()
                    ),
                });
            }
        }
        return Ok(());
    }

    let mut content = String::new();
    for oid in boundaries {
        content.push_str(oid);
        content.push('\n');
    }
    fs::write(&path, content).map_err(|source| FetchError::LocalState {
        message: format!(
            "failed to write shallow metadata '{}': {source}",
            path.display()
        ),
    })
}

fn apply_shallow_updates(shallow: &[String], unshallow: &[String]) -> Result<(), FetchError> {
    if shallow.is_empty() && unshallow.is_empty() {
        return Ok(());
    }

    let mut boundaries = read_shallow_boundaries()?;
    for oid in shallow {
        ObjectHash::from_str(oid).map_err(|source| FetchError::LocalState {
            message: format!("remote sent invalid shallow boundary '{oid}': {source}"),
        })?;
        boundaries.insert(oid.clone());
    }
    for oid in unshallow {
        ObjectHash::from_str(oid).map_err(|source| FetchError::LocalState {
            message: format!("remote sent invalid unshallow boundary '{oid}': {source}"),
        })?;
        boundaries.remove(oid);
    }
    write_shallow_boundaries(&boundaries)
}

/// Read-only counterpart of [`update_references`] for `--dry-run`: report the
/// remote-tracking ref updates the discovered refs would produce, without any
/// database writes.
async fn compute_fetch_ref_preview(
    remote_config: &RemoteConfig,
    refs: &[DiscRef],
) -> Result<Vec<FetchRefUpdate>, FetchError> {
    let mut updates = Vec::new();
    for reference in refs {
        // `--tags --dry-run`: preview only the tags that would be newly created
        // (absent locally). Up-to-date and conflicting tags are not previewed as
        // updates because fetch never clobbers them.
        if let Some(tag_name) = reference._ref.strip_prefix("refs/tags/") {
            if reference._ref.ends_with("^{}") {
                continue;
            }
            let existing =
                tag::find_tag_ref(tag_name)
                    .await
                    .map_err(|error| FetchError::UpdateRefs {
                        message: format!(
                            "failed to inspect existing tag '{}': {error}",
                            reference._ref
                        ),
                    })?;
            if existing.is_none() {
                updates.push(FetchRefUpdate {
                    remote_ref: reference._ref.clone(),
                    old_oid: None,
                    new_oid: reference._hash.clone(),
                    forced: false,
                });
            }
            continue;
        }

        let full_ref_name = if let Some(branch_name) = reference._ref.strip_prefix("refs/heads/") {
            format!("refs/remotes/{}/{}", remote_config.name, branch_name)
        } else if let Some(mr_name) = reference._ref.strip_prefix("refs/mr/") {
            format!("refs/remotes/{}/mr/{}", remote_config.name, mr_name)
        } else {
            continue;
        };

        let old_oid = Branch::find_branch_result(&full_ref_name, Some(&remote_config.name))
            .await
            .map_err(|error| FetchError::UpdateRefs {
                message: format!(
                    "failed to inspect existing remote-tracking ref '{full_ref_name}': {error}"
                ),
            })?
            .map(|branch| branch.commit.to_string());

        if old_oid.as_deref() == Some(reference._hash.as_str()) {
            continue;
        }
        updates.push(FetchRefUpdate {
            remote_ref: full_ref_name,
            old_oid,
            new_oid: reference._hash.clone(),
            forced: false,
        });
    }
    Ok(updates)
}

fn fetch_head_path() -> Result<PathBuf, FetchError> {
    util::try_get_storage_path(None)
        .map(|storage| storage.join("FETCH_HEAD"))
        .map_err(|source| FetchError::LocalState {
            message: format!("failed to locate repository storage for FETCH_HEAD: {source}"),
        })
}

/// Render the `FETCH_HEAD` body: one `<oid>\t<not-for-merge>\t<desc>` line per
/// fetched ref. Libra fetch never designates a merge target (merge with
/// `libra pull`), so every line is marked `not-for-merge`.
fn format_fetch_head(result: &FetchOutput) -> String {
    let mut lines = Vec::new();
    for remote in &result.remotes {
        let tracking_prefix = format!("refs/remotes/{}/", remote.remote);
        for update in &remote.refs_updated {
            if let Some(tag_name) = update.remote_ref.strip_prefix("refs/tags/") {
                lines.push(format!(
                    "{}\tnot-for-merge\ttag '{}' of {}",
                    update.new_oid, tag_name, remote.url
                ));
                continue;
            }
            let branch = update
                .remote_ref
                .strip_prefix(&tracking_prefix)
                .unwrap_or(&update.remote_ref);
            lines.push(format!(
                "{}\tnot-for-merge\tbranch '{}' of {}",
                update.new_oid, branch, remote.url
            ));
        }
    }
    lines.join("\n")
}

/// Write (or, with `append`, accumulate into) `.libra/FETCH_HEAD` via an atomic
/// temp-file + rename, owner-only on Unix.
fn write_fetch_head(result: &FetchOutput, append: bool) -> Result<(), FetchError> {
    let path = fetch_head_path()?;
    let body = format_fetch_head(result);

    let mut content = String::new();
    if append && let Ok(existing) = fs::read_to_string(&path) {
        content.push_str(&existing);
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
    }
    if !body.is_empty() {
        content.push_str(&body);
        content.push('\n');
    }

    let tmp = path.with_extension("tmp");
    fs::write(&tmp, &content).map_err(|source| FetchError::LocalState {
        message: format!(
            "failed to write FETCH_HEAD temp '{}': {source}",
            tmp.display()
        ),
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600)).map_err(|source| {
            FetchError::LocalState {
                message: format!("failed to set permissions on FETCH_HEAD: {source}"),
            }
        })?;
    }
    fs::rename(&tmp, &path).map_err(|source| FetchError::LocalState {
        message: format!(
            "failed to finalize FETCH_HEAD '{}': {source}",
            path.display()
        ),
    })
}

/// Resolve the short branch name a remote's HEAD points at. Prefers the
/// server's `symref=HEAD:refs/heads/<branch>` capability (exact, advertised by
/// `git-upload-pack`); otherwise matches HEAD's advertised OID against a branch
/// tip; otherwise falls back to `main`, then `master`, then the first branch.
/// Shared by `fetch` (to cache the remote HEAD) and `remote show` /
/// `remote set-head --auto`.
pub(crate) fn resolve_remote_default_branch(
    capabilities: &[String],
    ref_heads: &[DiscRef],
    remote_head: Option<&DiscRef>,
) -> Option<String> {
    // 1. `symref=HEAD:refs/heads/<branch>` capability.
    for cap in capabilities {
        if let Some(rest) = cap.strip_prefix("symref=HEAD:")
            && let Some(branch) = rest.strip_prefix("refs/heads/")
            && !branch.is_empty()
        {
            return Some(branch.to_string());
        }
    }
    // 2. Match HEAD's advertised OID against a branch tip.
    if let Some(remote_head) = remote_head
        && let Some(branch) = ref_heads
            .iter()
            .find(|r| r._hash == remote_head._hash)
            .and_then(|r| r._ref.strip_prefix("refs/heads/"))
    {
        return Some(branch.to_string());
    }
    // 3. Heuristic fallback: main, then master, then the first branch.
    if ref_heads.is_empty() {
        return None;
    }
    ref_heads
        .iter()
        .find(|r| r._ref == "refs/heads/main")
        .or_else(|| ref_heads.iter().find(|r| r._ref == "refs/heads/master"))
        .or(ref_heads.first())
        .and_then(|r| r._ref.strip_prefix("refs/heads/"))
        .map(str::to_owned)
}

/// Prune stale `refs/remotes/<name>/*` tracking refs after a fetch (`--prune`/`-p`).
///
/// Staleness is classified by [`classify_stale_tracking_branches`] — the same
/// rule `remote prune` uses: a local tracking ref is stale when the remote no
/// longer advertises a matching `refs/heads/*` / `refs/mr/*` ref.
/// `refs/remotes/<name>/HEAD`, local branches, tags, and every other remote are
/// never considered.
///
/// With `dry_run`, the stale refs are classified and returned but nothing is
/// written. Otherwise each stale ref is removed and a non-lossy audit reflog
/// entry (`<old> -> 0…0`) recorded **inside a single transaction**: a failure
/// part-way through rolls back every deletion, so the repository never ends up
/// in a partially-pruned state.
async fn prune_stale_remote_refs(
    remote_name: &str,
    remote_branch_names: &HashSet<String>,
    dry_run: bool,
) -> Result<Vec<FetchPruneEntry>, FetchError> {
    let local = Branch::list_branches_result(Some(remote_name))
        .await
        .map_err(|error| FetchError::UpdateRefs {
            message: format!(
                "failed to list remote-tracking refs for prune of '{remote_name}': {error}"
            ),
        })?;

    let pruned: Vec<FetchPruneEntry> =
        classify_stale_tracking_branches(remote_name, remote_branch_names, &local)
            .into_iter()
            .map(|entry: RemotePruneEntry| {
                let old_oid = local
                    .iter()
                    .find(|b| b.name == entry.remote_ref)
                    .map(|b| b.commit.to_string());
                FetchPruneEntry {
                    remote_ref: entry.remote_ref,
                    branch: entry.branch,
                    old_oid,
                }
            })
            .collect();

    if dry_run || pruned.is_empty() {
        return Ok(pruned);
    }

    let db = get_db_conn_instance().await;
    let remote_owned = remote_name.to_string();
    let to_delete = pruned.clone();
    let zero = ObjectHash::zero_str(get_hash_kind()).to_string();
    db.transaction(|txn| {
        Box::pin(async move {
            for entry in &to_delete {
                // Record a non-lossy audit entry before deleting the ref. The
                // reflog table is keyed by ref name (no FK to the reference
                // row), so the entry survives the ref's deletion and the prune
                // audit chain is not lost.
                let context = ReflogContext {
                    old_oid: entry.old_oid.clone().unwrap_or_else(|| zero.clone()),
                    new_oid: zero.clone(),
                    action: ReflogAction::Fetch,
                };
                Reflog::insert_single_entry(txn, &context, &entry.remote_ref)
                    .await
                    .map_err(|source| FetchError::UpdateRefs {
                        message: format!(
                            "failed to record prune reflog for '{}': {source}",
                            entry.remote_ref
                        ),
                    })?;
                Branch::delete_branch_result_with_conn(txn, &entry.remote_ref, Some(&remote_owned))
                    .await
                    .map_err(|source| FetchError::UpdateRefs {
                        message: format!(
                            "failed to prune stale remote-tracking ref '{}': {source}",
                            entry.remote_ref
                        ),
                    })?;
            }
            Ok::<_, FetchError>(())
        })
    })
    .await
    .map_err(|source| FetchError::UpdateRefs {
        message: match source {
            TransactionError::Connection(error) => error.to_string(),
            TransactionError::Transaction(error) => error.to_string(),
        },
    })?;

    Ok(pruned)
}

async fn update_references(
    remote_config: &RemoteConfig,
    refs: &[DiscRef],
    ref_heads: &[DiscRef],
    remote_head: Option<DiscRef>,
    branch: Option<String>,
    capabilities: Vec<String>,
) -> Result<Vec<FetchRefUpdate>, FetchError> {
    let db = get_db_conn_instance().await;
    let remote_config = remote_config.clone();
    let refs = refs.to_vec();
    let ref_heads = ref_heads.to_vec();
    db.transaction(|txn| {
        Box::pin(async move {
            let mut updates = Vec::new();
            for reference in &refs {
                // Tags are persisted separately by `persist_fetched_tags` (they
                // live in the shared `refs/tags/*` namespace and have their own
                // create/skip/clobber policy).
                if reference._ref.starts_with("refs/tags/") {
                    continue;
                }

                let full_ref_name: String;
                if let Some(branch_name) = reference._ref.strip_prefix("refs/heads/") {
                    full_ref_name = format!("refs/remotes/{}/{}", remote_config.name, branch_name);
                } else if let Some(mr_name) = reference._ref.strip_prefix("refs/mr/") {
                    full_ref_name = format!("refs/remotes/{}/mr/{}", remote_config.name, mr_name);
                } else {
                    tracing::debug!(
                        "Skipping unsupported ref type during fetch: {}",
                        reference._ref
                    );
                    continue;
                }

                let old_oid = Branch::find_branch_result_with_conn(
                    txn,
                    &full_ref_name,
                    Some(&remote_config.name),
                )
                .await
                .map_err(|error| FetchError::UpdateRefs {
                    message: format!(
                        "failed to inspect existing remote-tracking ref '{full_ref_name}': {error}"
                    ),
                })?
                .map(|branch| branch.commit.to_string());

                if old_oid.as_deref() == Some(reference._hash.as_str()) {
                    continue;
                }

                Branch::update_branch_with_conn(
                    txn,
                    &full_ref_name,
                    &reference._hash,
                    Some(&remote_config.name),
                )
                .await
                .map_err(|source| FetchError::UpdateRefs {
                    message: format!(
                        "failed to persist remote-tracking ref '{full_ref_name}': {source}"
                    ),
                })?;

                let context = ReflogContext {
                    old_oid: old_oid
                        .clone()
                        .unwrap_or_else(|| ObjectHash::zero_str(get_hash_kind()).to_string()),
                    new_oid: reference._hash.clone(),
                    action: ReflogAction::Fetch,
                };
                Reflog::insert_single_entry(txn, &context, &full_ref_name)
                    .await
                    .map_err(|source| FetchError::UpdateRefs {
                        message: format!(
                            "failed to record reflog for remote-tracking ref '{full_ref_name}': {source}"
                        ),
                    })?;
                updates.push(FetchRefUpdate {
                    remote_ref: full_ref_name,
                    forced: fetch_update_is_forced(old_oid.as_deref(), &reference._hash),
                    old_oid,
                    new_oid: reference._hash.clone(),
                });
            }

            // Update the cached remote HEAD to the branch it points at, resolved
            // via the shared helper (symref capability, else OID match, else
            // main/master/first).
            let remote_default_branch =
                resolve_remote_default_branch(&capabilities, &ref_heads, remote_head.as_ref());
            if let Some(branch_name) = remote_default_branch {
                Head::update_with_conn(txn, Head::Branch(branch_name), Some(&remote_config.name))
                    .await;
            } else if branch.is_none() && remote_head.is_some() {
                tracing::debug!("remote HEAD does not point to a branch ref");
            }

            Ok::<_, FetchError>(updates)
        })
    })
    .await
    .map_err(|source| FetchError::UpdateRefs {
        message: match source {
            TransactionError::Connection(error) => error.to_string(),
            TransactionError::Transaction(error) => error.to_string(),
        },
    })
}

/// Whether `new_oid` updating `old_oid` is a forced (non-fast-forward) change.
/// Best-effort: `false` for a new ref or when ancestry cannot be computed.
fn fetch_update_is_forced(old_oid: Option<&str>, new_oid: &str) -> bool {
    let Some(old_str) = old_oid else {
        return false;
    };
    let (Ok(old), Ok(new)) = (ObjectHash::from_str(old_str), ObjectHash::from_str(new_oid)) else {
        return false;
    };
    if old == new {
        return false;
    }
    !commit_is_ancestor(&old, &new)
}

/// True when `ancestor` is reachable from `descendant` by walking parents.
fn commit_is_ancestor(ancestor: &ObjectHash, descendant: &ObjectHash) -> bool {
    if ancestor == descendant {
        return true;
    }
    let mut queue = std::collections::VecDeque::new();
    let mut visited = HashSet::new();
    queue.push_back(*descendant);
    visited.insert(*descendant);
    while let Some(id) = queue.pop_front() {
        let Ok(commit) = load_object::<Commit>(&id) else {
            continue;
        };
        for parent in &commit.parent_commit_ids {
            if parent == ancestor {
                return true;
            }
            if visited.insert(*parent) {
                queue.push_back(*parent);
            }
        }
    }
    false
}

/// Resolve the effective [`TagFetchMode`] for `remote_name`: an explicit CLI
/// choice (`tag_cli`) wins; otherwise `remote.<name>.tagOpt` (`--tags` /
/// `--no-tags`); otherwise Git's default auto-follow.
async fn resolve_tag_mode(remote_name: &str, tag_cli: Option<TagFetchMode>) -> TagFetchMode {
    if let Some(mode) = tag_cli {
        return mode;
    }
    match ConfigKv::get(&format!("remote.{remote_name}.tagOpt")).await {
        Ok(Some(entry)) => match entry.value.trim() {
            "--tags" => TagFetchMode::All,
            "--no-tags" => TagFetchMode::NoTags,
            _ => TagFetchMode::AutoFollow,
        },
        _ => TagFetchMode::AutoFollow,
    }
}

/// Persist fetched tags into the shared `refs/tags/*` namespace (kind=Tag,
/// remote=None), matching Git. Policy: create when absent; skip when already at
/// the same target; on a conflicting local tag, clobber when `force`, else keep
/// the local tag with a warning. Tags carry no reflog. Returns one
/// [`FetchRefUpdate`] per created/clobbered tag.
async fn persist_fetched_tags(
    tags: &[DiscRef],
    force: bool,
) -> Result<Vec<FetchRefUpdate>, FetchError> {
    if tags.is_empty() {
        return Ok(Vec::new());
    }
    let db = get_db_conn_instance().await;
    let tags = tags.to_vec();
    db.transaction(|txn| {
        Box::pin(async move {
            let mut updates = Vec::new();
            for tag in &tags {
                if tag._ref.ends_with("^{}") || !tag._ref.starts_with("refs/tags/") {
                    continue;
                }
                let existing = ref_model::Entity::find()
                    .filter(ref_model::Column::Name.eq(tag._ref.clone()))
                    .filter(ref_model::Column::Kind.eq(ref_model::ConfigKind::Tag))
                    .one(txn)
                    .await
                    .map_err(|error| FetchError::UpdateRefs {
                        message: format!("failed to inspect existing tag '{}': {error}", tag._ref),
                    })?;
                match existing {
                    Some(row) if row.commit.as_deref() == Some(tag._hash.as_str()) => {
                        // Already up to date — nothing to report.
                    }
                    Some(row) if force => {
                        let old = row.commit.clone();
                        let mut active: ref_model::ActiveModel = row.into();
                        active.commit = Set(Some(tag._hash.clone()));
                        active
                            .update(txn)
                            .await
                            .map_err(|source| FetchError::UpdateRefs {
                                message: format!(
                                    "failed to force-update tag '{}': {source}",
                                    tag._ref
                                ),
                            })?;
                        updates.push(FetchRefUpdate {
                            remote_ref: tag._ref.clone(),
                            old_oid: old,
                            new_oid: tag._hash.clone(),
                            forced: true,
                        });
                    }
                    Some(_) => {
                        tracing::warn!(
                            "tag '{}' already exists with a different target; keeping the local tag (use --force to overwrite)",
                            tag._ref
                        );
                    }
                    None => {
                        let new_ref = ref_model::ActiveModel {
                            name: Set(Some(tag._ref.clone())),
                            kind: Set(ref_model::ConfigKind::Tag),
                            commit: Set(Some(tag._hash.clone())),
                            ..Default::default()
                        };
                        new_ref
                            .insert(txn)
                            .await
                            .map_err(|source| FetchError::UpdateRefs {
                                message: format!("failed to persist tag '{}': {source}", tag._ref),
                            })?;
                        updates.push(FetchRefUpdate {
                            remote_ref: tag._ref.clone(),
                            old_oid: None,
                            new_oid: tag._hash.clone(),
                            forced: false,
                        });
                    }
                }
            }
            Ok::<_, FetchError>(updates)
        })
    })
    .await
    .map_err(|source| FetchError::UpdateRefs {
        message: match source {
            TransactionError::Connection(error) => error.to_string(),
            TransactionError::Transaction(error) => error.to_string(),
        },
    })
}

/// Soft cap on the number of commits we walk back from each branch tip when
/// constructing the `have` list. Each `have` line is small, but we still want
/// to keep the request bounded for repos with deep history. Tips themselves
/// always go into `have` regardless of this limit so that the server can
/// recognise every local/remote-tracking branch as a potential common ancestor.
const HAVE_HISTORY_LIMIT: usize = 256;

/// Maximum chain length when peeling a (possibly tag-of-tag) annotated tag to
/// its target while building the `have` set. Bounds runaway/cyclic tag chains.
const MAX_TAG_PEEL_DEPTH: usize = 32;

async fn current_have_safe() -> Result<Vec<String>, FetchError> {
    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    struct QueueItem {
        priority: usize,
        commit: ObjectHash,
    }

    let mut c_pending = std::collections::BinaryHeap::new();
    let mut inserted = HashSet::new();
    let check_and_insert =
        |commit: &Commit,
         inserted: &mut HashSet<String>,
         c_pending: &mut std::collections::BinaryHeap<QueueItem>| {
            if inserted.contains(&commit.id.to_string()) {
                return;
            }
            inserted.insert(commit.id.to_string());
            c_pending.push(QueueItem {
                priority: commit.committer.timestamp,
                commit: commit.id,
            });
        };

    let mut remotes = ConfigKv::all_remote_configs()
        .await
        .map_err(|source| FetchError::LocalState {
            message: format!("failed to read remote configuration: {source}"),
        })?
        .iter()
        .map(|remote| Some(remote.name.to_owned()))
        .collect::<Vec<_>>();
    remotes.push(None);

    let mut have = Vec::new();
    let mut have_set: HashSet<String> = HashSet::new();
    let shallow_boundaries = read_shallow_boundaries()?;

    // Phase 1: every local + remote-tracking branch tip becomes a `have`,
    // unconditionally. These are the commits the server is most likely to
    // recognise as a common ancestor; dropping any of them forces the server
    // to re-send the pack regions reachable from those tips on every fetch
    // (the bug that made `libra pull` re-download the same pack repeatedly
    // on repos with more active branches than the previous traversal limit).
    for remote in &remotes {
        let branches = Branch::list_branches_result(remote.as_deref())
            .await
            .map_err(|source| FetchError::LocalState {
                message: format!("failed to list local branches: {source}"),
            })?;
        for branch in branches {
            let commit: Commit =
                load_object(&branch.commit).map_err(|source| FetchError::LocalState {
                    message: format!(
                        "failed to load local commit '{}': {}",
                        branch.commit, source
                    ),
                })?;
            check_and_insert(&commit, &mut inserted, &mut c_pending);
            let oid = branch.commit.to_string();
            if have_set.insert(oid.clone()) {
                have.push(oid);
            }
        }
    }

    // Phase 1b: local tags are `have` candidates too. Every tag ref tip (the
    // annotated tag object, or the commit/target for a lightweight tag) is
    // added, and annotated tags are peeled — best effort — so their target
    // commit seeds the parent walk. Without this, a `--tags` fetch re-downloads
    // the tag objects and their history on every run (the bug that previously
    // forced tag fetching to be backed out). This is unconditional: the client
    // genuinely has these objects, so advertising them is always correct.
    let db = get_db_conn_instance().await;
    let tag_rows = ref_model::Entity::find()
        .filter(ref_model::Column::Kind.eq(ref_model::ConfigKind::Tag))
        .all(&db)
        .await
        .map_err(|source| FetchError::LocalState {
            message: format!("failed to list local tags for have-set: {source}"),
        })?;
    for row in tag_rows {
        let Some(ref_oid) = row.commit else {
            continue;
        };
        if have_set.insert(ref_oid.clone()) {
            have.push(ref_oid.clone());
        }
        // Peel annotated tags best-effort: a missing object simply means this
        // tag contributes only its own oid. The bounded loop guards cycles.
        let Ok(mut current) = ObjectHash::from_str(&ref_oid) else {
            continue;
        };
        for _ in 0..MAX_TAG_PEEL_DEPTH {
            match tag::load_object_trait(&current).await {
                Ok(TagObject::Tag(inner)) => {
                    let target_oid = inner.object_hash.to_string();
                    if have_set.insert(target_oid.clone()) {
                        have.push(target_oid);
                    }
                    current = inner.object_hash;
                }
                Ok(TagObject::Commit(commit)) => {
                    check_and_insert(&commit, &mut inserted, &mut c_pending);
                    break;
                }
                // Tree/Blob targets, or a missing object: nothing more to seed.
                _ => break,
            }
        }
    }

    // Phase 2: walk parents in newest-first order to provide additional
    // common-ancestor candidates for divergent histories, bounded by
    // `HAVE_HISTORY_LIMIT` so very deep repos don't produce an unbounded
    // request body.
    while have.len() < HAVE_HISTORY_LIMIT && !c_pending.is_empty() {
        let Some(item) = c_pending.pop() else {
            break;
        };
        let oid = item.commit.to_string();
        if have_set.insert(oid.clone()) {
            have.push(oid);
        }
        if shallow_boundaries.contains(&item.commit.to_string()) {
            continue;
        }

        let commit: Commit =
            load_object(&item.commit).map_err(|source| FetchError::LocalState {
                message: format!("failed to load local commit '{}': {}", item.commit, source),
            })?;
        for parent in commit.parent_commit_ids {
            let parent_commit: Commit =
                load_object(&parent).map_err(|source| FetchError::LocalState {
                    message: format!("failed to load parent commit '{}': {}", parent, source),
                })?;
            check_and_insert(&parent_commit, &mut inserted, &mut c_pending);
        }
    }

    Ok(have)
}

/// Read 4 bytes hex number
async fn read_hex_4(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf).await?;
    let hex_str = std::str::from_utf8(&buf).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "invalid packet line header '{}'",
                String::from_utf8_lossy(&buf)
            ),
        )
    })?;
    u32::from_str_radix(hex_str, 16).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid packet line header '{hex_str}'"),
        )
    })
}

/// async version of `read_pkt_line`
/// - return (raw length, data)
async fn read_pkt_line(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<(usize, Vec<u8>)> {
    let len = read_hex_4(reader).await?;
    if len == 0 {
        return Ok((0, Vec::new()));
    }
    let mut data = vec![0u8; (len - 4) as usize];
    reader.read_exact(&mut data).await?;
    Ok((len as usize, data))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{Duration, SystemTime},
    };

    use bytes::{Bytes, BytesMut};
    use futures_util::{StreamExt, stream};
    use git_internal::hash::ObjectHash;

    #[test]
    fn resolve_fetch_timeout_env_millis_wins() {
        // A unique env var name so no concurrent real fetch reads it. The env
        // branch returns before any config read, keeping this deterministic.
        let var = "LIBRA_TEST_FETCH_TIMEOUT_ENV_WINS";
        // SAFETY: single-threaded set/read/remove within this test; the var name
        // is unique to this test so no other thread observes it.
        unsafe { std::env::set_var(var, "2500") };
        let resolved =
            super::resolve_fetch_timeout(None, "connectTimeout", var, Duration::from_secs(30));
        unsafe { std::env::remove_var(var) };
        assert_eq!(resolved, Duration::from_millis(2500));
    }

    #[test]
    fn resolve_fetch_timeout_ignores_unparseable_env() {
        let var = "LIBRA_TEST_FETCH_TIMEOUT_GARBAGE";
        // SAFETY: as above.
        unsafe { std::env::set_var(var, "not-a-number") };
        // Garbage env is ignored; with a config key nothing sets it falls to the
        // default (the unique key keeps this independent of the repo's config).
        let resolved = super::resolve_fetch_timeout(
            None,
            "connectTimeoutTestUnset",
            var,
            Duration::from_secs(9),
        );
        unsafe { std::env::remove_var(var) };
        assert_eq!(resolved, Duration::from_secs(9));
    }

    #[test]
    fn resolve_fetch_timeout_ignores_zero_env() {
        let var = "LIBRA_TEST_FETCH_TIMEOUT_ZERO";
        // SAFETY: as above. A `0` must not become a zero-duration timeout.
        unsafe { std::env::set_var(var, "0") };
        let resolved = super::resolve_fetch_timeout(
            None,
            "connectTimeoutTestUnset",
            var,
            Duration::from_secs(11),
        );
        unsafe { std::env::remove_var(var) };
        assert_eq!(resolved, Duration::from_secs(11));
    }
    use tempfile::tempdir;

    use super::{
        FetchError, PackCompletionTracker, RemoteProgressBuffer, RemoteSpecErrorKind,
        SSH_KEY_TEMP_FILE_MAX_AGE, apply_no_progress, cleanup_expired_vault_ssh_temp_files_in,
        ensure_vault_ssh_tmp_dir, parse_pack_entry_data_offset, read_be_u32, read_fetch_stream,
        redact_url_credentials,
    };
    use crate::{
        internal::protocol::FetchStream,
        utils::{
            output::{OutputConfig, ProgressMode, ProgressPreference},
            test::ScopedEnvVar,
        },
    };

    /// `--no-progress` forces progress reporting off while leaving progress on
    /// when the flag is absent (and short-circuits when it is already off).
    #[test]
    fn apply_no_progress_forces_progress_mode_off() {
        let text = OutputConfig {
            progress: ProgressMode::Text,
            ..OutputConfig::default()
        };
        let suppressed = apply_no_progress(&text, true).expect("Text + no_progress changes output");
        assert!(matches!(suppressed.progress, ProgressMode::None));
        assert!(matches!(
            suppressed.progress_preference,
            ProgressPreference::None
        ));

        // Without --no-progress the original config is kept (None returned).
        assert!(apply_no_progress(&text, false).is_none());

        // Already-off progress needs no change.
        let off = OutputConfig {
            progress: ProgressMode::None,
            ..OutputConfig::default()
        };
        assert!(apply_no_progress(&off, true).is_none());
    }

    #[test]
    fn resolve_remote_default_branch_prefers_symref_then_oid_then_heuristic() {
        use super::{DiscRef, resolve_remote_default_branch};
        let dr = |oid: &str, name: &str| DiscRef {
            _hash: oid.to_string(),
            _ref: name.to_string(),
        };
        let heads = vec![
            dr("aaa", "refs/heads/master"),
            dr("bbb", "refs/heads/dev"),
            dr("ccc", "refs/heads/main"),
        ];
        // 1. `symref=HEAD:` capability wins, even over a conflicting HEAD OID.
        let caps = vec![
            "side-band-64k".to_string(),
            "symref=HEAD:refs/heads/dev".to_string(),
        ];
        let head_main = dr("ccc", "HEAD");
        assert_eq!(
            resolve_remote_default_branch(&caps, &heads, Some(&head_main)).as_deref(),
            Some("dev")
        );
        // 2. No symref -> match HEAD's advertised OID against a branch tip.
        let head_master = dr("aaa", "HEAD");
        assert_eq!(
            resolve_remote_default_branch(&[], &heads, Some(&head_master)).as_deref(),
            Some("master")
        );
        // 3. No symref and HEAD OID matches nothing -> `main` heuristic.
        let head_unknown = dr("zzz", "HEAD");
        assert_eq!(
            resolve_remote_default_branch(&[], &heads, Some(&head_unknown)).as_deref(),
            Some("main")
        );
        // 4. No branches at all -> None.
        assert_eq!(resolve_remote_default_branch(&[], &[], None), None);
    }

    #[test]
    fn format_fetch_porcelain_layout_is_space_separated() {
        use super::{FetchOutput, FetchRefUpdate, FetchRepositoryResult, format_fetch_porcelain};

        let output = FetchOutput {
            all: false,
            requested_remote: Some("origin".to_string()),
            refspec: None,
            remotes: vec![FetchRepositoryResult {
                remote: "origin".to_string(),
                url: "https://example.com/x.git".to_string(),
                objects_fetched: 2,
                bytes_received: 64,
                refs_updated: vec![
                    FetchRefUpdate {
                        remote_ref: "refs/remotes/origin/main".to_string(),
                        old_oid: Some("a".repeat(40)),
                        new_oid: "b".repeat(40),
                        forced: false,
                    },
                    FetchRefUpdate {
                        remote_ref: "refs/remotes/origin/dev".to_string(),
                        old_oid: None,
                        new_oid: "c".repeat(40),
                        forced: false,
                    },
                ],
                pruned: Vec::new(),
            }],
        };

        let rendered = format_fetch_porcelain(&output);
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines.len(), 2);
        // Updated ref: the single-char flag is a space, so the line begins with
        // two spaces (`<flag> <old> <new> <ref>`). Dropping empty fields yields
        // the three data columns.
        let cols: Vec<&str> = lines[0].split(' ').filter(|c| !c.is_empty()).collect();
        assert_eq!(cols.len(), 3);
        assert_eq!(cols[0], "a".repeat(40));
        assert_eq!(cols[1], "b".repeat(40));
        assert_eq!(cols[2], "refs/remotes/origin/main");
        assert!(lines[0].starts_with("  "), "fast-forward flag is a space");
        // New ref: `*` flag with an all-zero old oid sized to the hash kind.
        assert!(lines[1].starts_with("* "));
        assert!(lines[1].contains(&"0".repeat(40)));
        assert!(lines[1].ends_with("refs/remotes/origin/dev"));
    }

    #[test]
    fn format_fetch_head_marks_every_ref_not_for_merge() {
        use super::{FetchOutput, FetchRefUpdate, FetchRepositoryResult, format_fetch_head};

        let output = FetchOutput {
            all: false,
            requested_remote: Some("origin".to_string()),
            refspec: None,
            remotes: vec![FetchRepositoryResult {
                remote: "origin".to_string(),
                url: "https://example.com/x.git".to_string(),
                objects_fetched: 1,
                bytes_received: 32,
                refs_updated: vec![FetchRefUpdate {
                    remote_ref: "refs/remotes/origin/main".to_string(),
                    old_oid: None,
                    new_oid: "d".repeat(40),
                    forced: false,
                }],
                pruned: Vec::new(),
            }],
        };

        let body = format_fetch_head(&output);
        assert!(body.contains(&"d".repeat(40)));
        assert!(body.contains("\tnot-for-merge\t"));
        // The tracking prefix is stripped to the bare branch name in the desc.
        assert!(body.contains("branch 'main' of https://example.com/x.git"));
    }

    /// `--prune` renders a `- <old> <zero> <ref>` porcelain line, structurally
    /// isomorphic with the update lines, and pruned refs never leak into the
    /// FETCH_HEAD body (which only records fetched refs).
    #[test]
    fn fetch_prune_porcelain_and_fetch_head_layout() {
        use super::{
            FetchOutput, FetchPruneEntry, FetchRepositoryResult, format_fetch_head,
            format_fetch_porcelain,
        };

        let output = FetchOutput {
            all: false,
            requested_remote: Some("origin".to_string()),
            refspec: None,
            remotes: vec![FetchRepositoryResult {
                remote: "origin".to_string(),
                url: "https://example.com/x.git".to_string(),
                objects_fetched: 0,
                bytes_received: 0,
                refs_updated: Vec::new(),
                pruned: vec![FetchPruneEntry {
                    remote_ref: "refs/remotes/origin/gone".to_string(),
                    branch: "origin/gone".to_string(),
                    old_oid: Some("e".repeat(40)),
                }],
            }],
        };

        let porcelain = format_fetch_porcelain(&output);
        let cols: Vec<&str> = porcelain.split(' ').filter(|c| !c.is_empty()).collect();
        assert!(porcelain.starts_with("- "), "pruned flag is `-`");
        assert_eq!(cols.len(), 4, "<flag> <old> <new> <ref>");
        assert_eq!(cols[0], "-");
        assert_eq!(cols[1], "e".repeat(40));
        assert_eq!(
            cols[2],
            "0".repeat(40),
            "deleted ref has an all-zero new-oid"
        );
        assert_eq!(cols[3], "refs/remotes/origin/gone");

        // Pruned refs are never fetched, so they must not appear in FETCH_HEAD.
        let body = format_fetch_head(&output);
        assert!(
            !body.contains("gone"),
            "pruned ref leaked into FETCH_HEAD: {body:?}"
        );
    }

    /// The porcelain prune row uses a hash-kind-correct zero id even when the
    /// pruned ref's old object id is unavailable: a SHA-256 repo emits 64 zeros,
    /// not a hardcoded 40. `#[serial]` because it mutates the process-global
    /// hash kind.
    #[test]
    #[serial_test::serial]
    fn fetch_prune_porcelain_zero_oid_is_hash_kind_aware() {
        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        use super::{FetchOutput, FetchPruneEntry, FetchRepositoryResult, format_fetch_porcelain};

        let _guard = set_hash_kind_for_test(HashKind::Sha256);
        let output = FetchOutput {
            all: false,
            requested_remote: Some("origin".to_string()),
            refspec: None,
            remotes: vec![FetchRepositoryResult {
                remote: "origin".to_string(),
                url: "https://example.com/x.git".to_string(),
                objects_fetched: 0,
                bytes_received: 0,
                refs_updated: Vec::new(),
                pruned: vec![FetchPruneEntry {
                    remote_ref: "refs/remotes/origin/gone".to_string(),
                    branch: "origin/gone".to_string(),
                    old_oid: None,
                }],
            }],
        };
        let porcelain = format_fetch_porcelain(&output);
        let cols: Vec<&str> = porcelain.split(' ').filter(|c| !c.is_empty()).collect();
        assert_eq!(cols.len(), 4, "<flag> <old> <new> <ref>");
        assert_eq!(cols[0], "-");
        assert_eq!(cols[1], "0".repeat(64), "SHA-256 old-oid zero is 64 hex");
        assert_eq!(cols[2], "0".repeat(64), "SHA-256 new-oid zero is 64 hex");
        assert_eq!(cols[3], "refs/remotes/origin/gone");
    }

    /// Pin the `Display` format for the static-message and direct-message
    /// variants of [`FetchError`]. These strings are used as the
    /// `CliError` message via `From<FetchError> for CliError` and
    /// surface in both human and `--json` envelopes for `fetch`, `clone`,
    /// and `pull`.
    ///
    /// Source-chained variants (Discovery, FetchObjects, PacketRead,
    /// ObjectsDirNotFound, PackDirCreate, PackWrite, IndexPack) wrap
    /// upstream io::Error / GitError types and are intentionally
    /// skipped — their `{source}` slot is owned by the wrapped type.
    #[test]
    fn fetch_error_display_pins_static_message_variants() {
        // InvalidRemoteSpec echoes the `reason` field verbatim.
        assert_eq!(
            FetchError::InvalidRemoteSpec {
                spec: "/missing/repo".to_string(),
                kind: RemoteSpecErrorKind::MissingLocalRepo,
                reason: "local path does not exist".to_string(),
            }
            .to_string(),
            "local path does not exist",
        );
        assert_eq!(
            FetchError::ObjectFormatMismatch {
                remote: git_internal::hash::HashKind::Sha1,
                local: git_internal::hash::HashKind::Sha256,
            }
            .to_string(),
            "remote object format 'sha1' does not match local 'sha256'",
        );
        assert_eq!(
            FetchError::RemoteBranchNotFound {
                branch: "feature".to_string(),
                remote: "origin".to_string(),
            }
            .to_string(),
            "remote branch feature not found in upstream origin",
        );
        assert_eq!(
            FetchError::InvalidPktHeader {
                header: "zzzz".to_string(),
            }
            .to_string(),
            "invalid packet line header 'zzzz'",
        );
        assert_eq!(
            FetchError::RemoteSideband {
                message: "access denied".to_string(),
            }
            .to_string(),
            "remote reported an error: access denied",
        );
        assert_eq!(
            FetchError::ChecksumMismatch.to_string(),
            "pack checksum mismatch",
        );
        assert_eq!(
            FetchError::UpdateRefs {
                message: "ref database is read-only".to_string(),
            }
            .to_string(),
            "failed to update references after fetch: ref database is read-only",
        );
        assert_eq!(
            FetchError::LocalState {
                message: "missing object directory".to_string(),
            }
            .to_string(),
            "failed to inspect local repository state: missing object directory",
        );
    }

    fn append_pkt_line(buf: &mut BytesMut, payload: &[u8]) {
        let len = payload.len() + 4;
        buf.extend_from_slice(format!("{len:04x}").as_bytes());
        buf.extend_from_slice(payload);
    }

    fn empty_pack_bytes() -> Vec<u8> {
        let mut pack = Vec::new();
        pack.extend_from_slice(b"PACK");
        pack.extend_from_slice(&2_u32.to_be_bytes());
        pack.extend_from_slice(&0_u32.to_be_bytes());
        let checksum = ObjectHash::new(&pack);
        pack.extend_from_slice(checksum.as_ref());
        pack
    }

    #[tokio::test]
    async fn read_fetch_stream_accepts_eof_after_complete_pack_without_flush() {
        let pack = empty_pack_bytes();
        let mut response = BytesMut::new();
        append_pkt_line(&mut response, b"NAK\n");

        let mut sideband = Vec::with_capacity(pack.len() + 1);
        sideband.push(1);
        sideband.extend_from_slice(&pack);
        append_pkt_line(&mut response, &sideband);

        let mut stream: FetchStream =
            stream::iter(vec![Ok::<Bytes, std::io::Error>(response.freeze())]).boxed();
        let output = OutputConfig::default();

        let data = read_fetch_stream(&mut stream, &output, "fetch origin")
            .await
            .expect("EOF after a complete pack should finish the fetch stream");

        assert_eq!(data.pack_data, pack);
    }

    #[tokio::test]
    async fn read_fetch_stream_rejects_a_truncated_pack() {
        // A valid pack with its trailing checksum chopped off: the stream reaches
        // the pack but it never completes, so it must surface as an explicit
        // `IncompletePack` error rather than being handed downstream as success.
        let mut pack = empty_pack_bytes();
        pack.truncate(pack.len() - 5);
        let mut response = BytesMut::new();
        append_pkt_line(&mut response, b"NAK\n");
        let mut sideband = vec![1u8];
        sideband.extend_from_slice(&pack);
        append_pkt_line(&mut response, &sideband);

        let mut stream: FetchStream =
            stream::iter(vec![Ok::<Bytes, std::io::Error>(response.freeze())]).boxed();
        let output = OutputConfig::default();

        let result = read_fetch_stream(&mut stream, &output, "fetch origin").await;
        let is_incomplete = matches!(&result, Err(super::FetchError::IncompletePack { .. }));
        assert!(
            is_incomplete,
            "a truncated pack must surface as IncompletePack, got: {}",
            result
                .err()
                .map_or_else(|| "Ok(..)".to_string(), |e| e.to_string())
        );
    }

    #[test]
    fn clean_sideband_message_strips_err_and_fatal_markers() {
        assert_eq!(
            super::clean_sideband_message(b"ERR access denied"),
            "access denied"
        );
        assert_eq!(
            super::clean_sideband_message(b"FATAL: repository not found"),
            "repository not found"
        );
        // A message without a marker is passed through (trimmed) unchanged.
        assert_eq!(
            super::clean_sideband_message(b"  upload-pack: not our ref  "),
            "upload-pack: not our ref"
        );
    }

    #[tokio::test]
    async fn read_fetch_stream_finishes_complete_pack_when_transport_stays_open() {
        let pack = empty_pack_bytes();
        let mut response = BytesMut::new();
        append_pkt_line(&mut response, b"NAK\n");

        let mut sideband = Vec::with_capacity(pack.len() + 1);
        sideband.push(1);
        sideband.extend_from_slice(&pack);
        append_pkt_line(&mut response, &sideband);

        let mut stream: FetchStream =
            stream::iter(vec![Ok::<Bytes, std::io::Error>(response.freeze())])
                .chain(stream::pending())
                .boxed();
        let output = OutputConfig::default();

        let data = tokio::time::timeout(
            Duration::from_millis(250),
            read_fetch_stream(&mut stream, &output, "fetch origin"),
        )
        .await
        .expect("complete pack should not wait for transport EOF or flush")
        .expect("complete pack should finish the fetch stream");

        assert_eq!(data.pack_data, pack);
    }

    #[tokio::test]
    async fn read_fetch_stream_finishes_non_empty_pack_when_transport_stays_open() {
        let pack = include_bytes!("../../tests/data/packs/small-sha1.pack").to_vec();
        let mut response = BytesMut::new();
        append_pkt_line(&mut response, b"NAK\n");

        let mut sideband = Vec::with_capacity(pack.len() + 1);
        sideband.push(1);
        sideband.extend_from_slice(&pack);
        append_pkt_line(&mut response, &sideband);

        let mut stream: FetchStream =
            stream::iter(vec![Ok::<Bytes, std::io::Error>(response.freeze())])
                .chain(stream::pending())
                .boxed();
        let output = OutputConfig::default();

        let data = tokio::time::timeout(
            Duration::from_millis(250),
            read_fetch_stream(&mut stream, &output, "fetch origin"),
        )
        .await
        .expect("complete non-empty pack should not wait for transport EOF or flush")
        .expect("complete non-empty pack should finish the fetch stream");

        assert_eq!(data.pack_data, pack);
    }

    /// Drive `RemoteProgressBuffer` with `payload` and return
    /// `(permanent_lines, transient_lines)` in dispatch order.
    fn collect_buffered_progress(
        buffer: &mut RemoteProgressBuffer,
        payload: &[u8],
    ) -> (Vec<String>, Vec<String>) {
        let mut perm = Vec::new();
        let mut trans = Vec::new();
        buffer.push(
            payload,
            |line| perm.push(line.to_string()),
            |line| trans.push(line.to_string()),
        );
        (perm, trans)
    }

    /// `\n`-terminated chunks are promoted to permanent log lines so the
    /// remote's `Counting objects: 100% (38/38), done.` survives above the bar.
    #[test]
    fn remote_progress_buffer_promotes_newline_terminated_lines() {
        let mut buffer = RemoteProgressBuffer::default();
        let (perm, trans) =
            collect_buffered_progress(&mut buffer, b"Counting objects: 100% (38/38), done.\n");

        assert_eq!(perm, vec!["Counting objects: 100% (38/38), done."]);
        assert!(trans.is_empty());
    }

    /// `\r`-terminated chunks update the bar message in place so successive
    /// `Counting objects:  5%\rCounting objects: 10%\r…` updates replace each
    /// other instead of stacking as separate lines.
    #[test]
    fn remote_progress_buffer_routes_carriage_returns_to_transient() {
        let mut buffer = RemoteProgressBuffer::default();
        let (perm, trans) = collect_buffered_progress(
            &mut buffer,
            b"Counting objects:  5%\rCounting objects: 10%\r",
        );

        assert!(perm.is_empty());
        assert_eq!(
            trans,
            vec!["Counting objects:  5%", "Counting objects: 10%"]
        );
    }

    /// Side-band chunks may split mid-word; partial bytes must survive until
    /// the next push delivers the terminator.
    #[test]
    fn remote_progress_buffer_holds_partial_bytes_across_pushes() {
        let mut buffer = RemoteProgressBuffer::default();
        let (perm1, trans1) = collect_buffered_progress(&mut buffer, b"Counting");
        assert!(perm1.is_empty());
        assert!(trans1.is_empty());

        let (perm2, trans2) = collect_buffered_progress(&mut buffer, b" objects: 100%, done.\n");
        assert_eq!(perm2, vec!["Counting objects: 100%, done."]);
        assert!(trans2.is_empty());
    }

    /// CRLF must collapse to a single permanent line so we don't emit a
    /// spurious empty transient followed by an empty permanent.
    #[test]
    fn remote_progress_buffer_treats_crlf_as_single_newline() {
        let mut buffer = RemoteProgressBuffer::default();
        let (perm, trans) = collect_buffered_progress(&mut buffer, b"Compressing done.\r\n");

        assert_eq!(perm, vec!["Compressing done."]);
        assert!(trans.is_empty());
    }

    /// At end of stream any unterminated tail must be flushed so a remote
    /// that closes mid-line still surfaces the partial message.
    #[test]
    fn remote_progress_buffer_flush_remaining_emits_trailing_partial() {
        let mut buffer = RemoteProgressBuffer::default();
        collect_buffered_progress(&mut buffer, b"Resolving deltas: 99%");
        let mut tail = Vec::new();
        buffer.flush_remaining(|line| tail.push(line.to_string()));

        assert_eq!(tail, vec!["Resolving deltas: 99%"]);
    }

    /// Empty payloads (e.g. a bare side-band code with no body) must not push
    /// anything through the line splitter.
    #[test]
    fn remote_progress_buffer_ignores_empty_payload() {
        let mut buffer = RemoteProgressBuffer::default();
        let (perm, trans) = collect_buffered_progress(&mut buffer, b"");
        assert!(perm.is_empty());
        assert!(trans.is_empty());
    }

    #[test]
    fn redact_url_credentials_strips_file_url_userinfo() {
        let redacted = redact_url_credentials("file://user:secret@example.com/repo.git");

        assert_eq!(redacted, "file://example.com/repo.git");
    }

    #[test]
    fn test_cleanup_expired_vault_ssh_temp_files_removes_old_tmp_files() {
        let temp_home = tempdir().expect("failed to create temp home");
        let tmp_dir = temp_home.path().join(".libra").join("tmp");
        fs::create_dir_all(&tmp_dir).expect("failed to create SSH temp dir");

        let expired = tmp_dir.join("ssh-key-old.tmp");
        fs::write(&expired, "secret").expect("failed to write expired temp file");

        let removed = cleanup_expired_vault_ssh_temp_files_in(
            &tmp_dir,
            SystemTime::now() + SSH_KEY_TEMP_FILE_MAX_AGE + Duration::from_secs(1),
        )
        .expect("cleanup should succeed");

        assert_eq!(removed, 1);
        assert!(!expired.exists(), "expired temp file should be removed");
    }

    #[test]
    fn test_cleanup_expired_vault_ssh_temp_files_keeps_fresh_and_non_tmp_files() {
        let temp_home = tempdir().expect("failed to create temp home");
        let tmp_dir = temp_home.path().join(".libra").join("tmp");
        fs::create_dir_all(&tmp_dir).expect("failed to create SSH temp dir");

        let fresh = tmp_dir.join("ssh-key-fresh.tmp");
        let keep = tmp_dir.join("note.txt");
        fs::write(&fresh, "secret").expect("failed to write fresh temp file");
        fs::write(&keep, "keep").expect("failed to write non-temp file");

        let removed = cleanup_expired_vault_ssh_temp_files_in(&tmp_dir, SystemTime::now())
            .expect("cleanup should succeed");

        assert_eq!(removed, 0);
        assert!(fresh.exists(), "fresh temp file should remain");
        assert!(keep.exists(), "non-temp file should remain");
    }

    #[test]
    fn test_ensure_vault_ssh_tmp_dir_uses_home_directory() {
        let temp_home = tempdir().expect("failed to create temp home");
        let _home = ScopedEnvVar::set("HOME", temp_home.path());
        let _userprofile = ScopedEnvVar::set("USERPROFILE", temp_home.path());

        let tmp_dir = ensure_vault_ssh_tmp_dir().expect("tmp dir should be created");

        assert_eq!(tmp_dir, temp_home.path().join(".libra").join("tmp"));
        assert!(tmp_dir.exists(), "tmp dir should exist");
    }

    #[test]
    fn test_update_refs_branch_lookup_error_is_preserved_in_message() {
        let error = FetchError::UpdateRefs {
            message: format!(
                "failed to inspect existing remote-tracking ref 'refs/remotes/origin/main': {}",
                crate::internal::branch::BranchStoreError::Corrupt {
                    name: "refs/remotes/origin/main".to_string(),
                    detail: "invalid object id".to_string(),
                }
            ),
        };

        assert!(
            error
                .to_string()
                .contains("stored branch reference 'refs/remotes/origin/main' is corrupt"),
            "unexpected fetch error: {error}"
        );
    }

    /// `read_be_u32` returns `None` for any range that would overflow
    /// the input slice; it must not panic. Pins the `offset + 4 > len`
    /// short-circuit added with `PackCompletionTracker` in v0.17.1060.
    #[test]
    fn read_be_u32_decodes_big_endian_and_short_circuits_on_overflow() {
        // Happy path: 4 BE bytes at offset 0.
        assert_eq!(read_be_u32(&[0x00, 0x00, 0x00, 0x05], 0), Some(5));
        assert_eq!(read_be_u32(&[0xDE, 0xAD, 0xBE, 0xEF], 0), Some(0xDEAD_BEEF));
        // Happy path: 4 BE bytes at a non-zero offset.
        assert_eq!(read_be_u32(&[0xAA, 0x00, 0x00, 0x00, 0x07], 1), Some(7));
        // Short input: only 3 bytes available at offset 0.
        assert_eq!(read_be_u32(&[0x00, 0x00, 0x00], 0), None);
        // Offset past end.
        assert_eq!(read_be_u32(&[0x00, 0x00, 0x00, 0x00], 4), None);
        // Empty input.
        assert_eq!(read_be_u32(&[], 0), None);
    }

    /// `PackCompletionTracker::read_header` accepts well-formed PACK v2
    /// and v3 headers and rejects everything else without panicking.
    /// The state mutations (`object_count`, `offset`) are part of the
    /// public contract that `observe` relies on, so pin them here.
    #[test]
    fn pack_completion_tracker_read_header_validates_magic_version_and_state() {
        // Reject: empty input.
        let mut tracker = PackCompletionTracker::default();
        assert!(!tracker.read_header(&[]));
        assert_eq!(tracker.object_count, None);

        // Reject: less than 12 bytes (header is exactly 12).
        let mut tracker = PackCompletionTracker::default();
        let short = [b'P', b'A', b'C', b'K', 0, 0, 0, 2, 0, 0, 0];
        assert!(!tracker.read_header(&short));
        assert_eq!(tracker.object_count, None);

        // Reject: wrong magic bytes.
        let mut tracker = PackCompletionTracker::default();
        let mut bad_magic = b"PACX".to_vec();
        bad_magic.extend_from_slice(&2_u32.to_be_bytes());
        bad_magic.extend_from_slice(&0_u32.to_be_bytes());
        assert!(!tracker.read_header(&bad_magic));

        // Reject: unsupported version (1 — packs predate widespread use).
        let mut tracker = PackCompletionTracker::default();
        let mut bad_version = b"PACK".to_vec();
        bad_version.extend_from_slice(&1_u32.to_be_bytes());
        bad_version.extend_from_slice(&0_u32.to_be_bytes());
        assert!(!tracker.read_header(&bad_version));

        // Reject: unsupported version (4).
        let mut tracker = PackCompletionTracker::default();
        let mut bad_version = b"PACK".to_vec();
        bad_version.extend_from_slice(&4_u32.to_be_bytes());
        bad_version.extend_from_slice(&0_u32.to_be_bytes());
        assert!(!tracker.read_header(&bad_version));

        // Accept: PACK v2 with 0 objects; `offset` advances to 12.
        let mut tracker = PackCompletionTracker::default();
        let mut empty_v2 = b"PACK".to_vec();
        empty_v2.extend_from_slice(&2_u32.to_be_bytes());
        empty_v2.extend_from_slice(&0_u32.to_be_bytes());
        assert!(tracker.read_header(&empty_v2));
        assert_eq!(tracker.object_count, Some(0));
        assert_eq!(tracker.offset, 12);

        // Accept: PACK v3 with 7 objects.
        let mut tracker = PackCompletionTracker::default();
        let mut seven_v3 = b"PACK".to_vec();
        seven_v3.extend_from_slice(&3_u32.to_be_bytes());
        seven_v3.extend_from_slice(&7_u32.to_be_bytes());
        assert!(tracker.read_header(&seven_v3));
        assert_eq!(tracker.object_count, Some(7));
        assert_eq!(tracker.offset, 12);
    }

    /// `parse_pack_entry_data_offset` returns `None` when the entry
    /// header runs past the end of the slice and `Some(offset)`
    /// pointing past the variable-length size header for the
    /// happy-path object types (1..=4 = commit/tree/blob/tag).
    #[test]
    fn parse_pack_entry_data_offset_returns_data_start_for_simple_object() {
        // Single byte header: type=3 (blob = 0b011), size <= 15, no
        // continuation bit. First byte: 0b0_011_0000 = 0x30 (size 0).
        // `data_offset` should equal `offset + 1`.
        let entry = [0x30_u8, 0x78, 0x9C]; // 0x78 0x9C = zlib stream begin
        assert_eq!(parse_pack_entry_data_offset(&entry, 0, 20), Some(1));

        // Two-byte size header: first byte has continuation bit set
        // (0b1_011_0000 = 0xB0), second byte is the last size chunk
        // (0b0_0000001 = 0x01). data_offset = 2.
        let entry = [0xB0_u8, 0x01, 0x78, 0x9C];
        assert_eq!(parse_pack_entry_data_offset(&entry, 0, 20), Some(2));

        // Reject: header truncated mid-continuation.
        let entry = [0xB0_u8]; // says "continue" but nothing follows
        assert_eq!(parse_pack_entry_data_offset(&entry, 0, 20), None);

        // Reject: unknown object type (5 is reserved, not 1..=4 / 6 / 7).
        let entry = [0x50_u8]; // 0b0_101_0000 = type 5
        assert_eq!(parse_pack_entry_data_offset(&entry, 0, 20), None);
    }
}
