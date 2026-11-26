//! Trust / provenance store for external `libra-agent-*` binaries (AG-18).
//!
//! External binaries are quarantined by default: discovery never registers
//! them as callable, and `rpc invoke` refuses until the operator records a
//! trust entry with `libra agent rpc trust <slug>`. A trust record pins the
//! binary's canonical path plus provenance markers (sha256, device, inode,
//! mtime); every subsequent invoke revalidates them and any drift revokes
//! the record fail-closed (`LBR-AGENT-005`).
//!
//! TOCTOU note: Rust's `std::process::Command` cannot portably exec from an
//! already-verified file descriptor, so this module implements the
//! best-effort mitigation tier from `docs/development/tracing/agent.md`
//! (威胁 T9 / 强制补强项 #2): canonical absolute path, parent directory not
//! world-writable, sha256 + device/inode/mtime revalidation immediately
//! before spawn, and quarantine on any mismatch. This narrows but does not
//! eliminate the check-to-exec race; fd-derived exec is future work.

use std::{
    fs,
    io::Read as _,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use anyhow::anyhow;
use anyhow::{Context, Result, bail};
use sea_orm::ConnectionTrait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::internal::config::ConfigKv;
#[cfg(feature = "storage-sqlite")]
use crate::internal::db::get_db_conn_instance;

/// Config key prefix for trust records (one JSON value per slug).
const TRUST_KEY_PREFIX: &str = "agent.trust.";

/// Settings gate for the whole external-agent surface (E2). While this is
/// off (the default) every `agent rpc` entry point that touches external
/// binaries — `list` discovery, `trust`, `invoke` — refuses with
/// `LBR-AGENT-002`; only `untrust` (which strictly tightens security)
/// stays available. Key spelling follows the settings table in
/// `docs/development/tracing/agent.md`.
pub const EXTERNAL_AGENTS_ENABLED_KEY: &str = "agent.external_agents.enabled";

/// A0-08: trusted-directory allowlist (E2). A binary is only trustable when
/// its canonical path lives under one of these directories — rejecting
/// arbitrary user-writable `$PATH` entries. Stored as a JSON string array in a
/// single `config_kv` cell; `libra agent rpc trust --dir <path>` appends to it.
pub const TRUSTED_DIRS_KEY: &str = "agent.external_agents.trusted_dirs";

/// A0-08: extra environment variables (exact names) to pass through to spawned
/// external agents on top of [`super::rpc::RPC_ENV_PASSTHROUGH_ALLOWLIST`].
/// Credential/endpoint names are always rejected (see [`env_name_is_forbidden`]).
pub const ENV_ALLOWLIST_EXTRA_KEY: &str = "agent.external_agents.env_allowlist_extra";

/// A0-08: the default trusted directory when the operator has registered none.
/// `~` is expanded against `$HOME` at read time.
pub const DEFAULT_TRUSTED_DIRS: &[&str] = &["~/.libra/agents"];

/// One recorded trust decision for an external binary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrustRecord {
    pub path: PathBuf,
    /// Lowercase hex sha256 of the binary contents.
    pub sha256: String,
    pub device: u64,
    pub inode: u64,
    /// mtime as unix seconds (best-effort; 0 when unavailable).
    pub mtime: i64,
}

/// Provenance markers computed from the binary on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct Provenance {
    pub canonical_path: PathBuf,
    pub sha256: String,
    pub device: u64,
    pub inode: u64,
    pub mtime: i64,
}

/// Whether the operator has opted in to external `libra-agent-*` agents.
/// Absent or non-true values mean disabled (fail-closed default).
pub async fn external_agents_enabled() -> Result<bool> {
    let entry = ConfigKv::get(EXTERNAL_AGENTS_ENABLED_KEY)
        .await
        .context("read agent.external_agents.enabled")?;
    Ok(entry
        .map(|e| {
            let v = e.value.trim().to_ascii_lowercase();
            v == "true" || v == "1" || v == "yes" || v == "on"
        })
        .unwrap_or(false))
}

/// Compute the provenance markers for `path` (canonicalizes first).
///
/// The bytes are hashed in one streaming pass over the opened descriptor, so
/// memory use does not grow with the binary and the digest equals a
/// whole-file sha256 (records written by earlier releases stay valid). There
/// is deliberately no fixed size cap: real provider CLIs are large single
/// files (the OpenCode Bun build is ~171 MiB) and must stay trustable. The
/// read is bounded by the descriptor's own stat length instead, so a file
/// that yields more bytes than it reported (one being appended to, or a
/// device such as `/dev/zero`) is refused rather than hashed without end.
pub fn compute_provenance(path: &Path) -> Result<Provenance> {
    let canonical_path = path
        .canonicalize()
        .with_context(|| format!("canonicalize external agent binary {}", path.display()))?;
    let mut file = fs::File::open(&canonical_path)
        .with_context(|| format!("open external agent binary {}", canonical_path.display()))?;
    // Stat the opened file, not the pathname again: a concurrent replacement
    // cannot make the provenance markers describe bytes other than the bytes
    // we actually hash below.
    let meta = file
        .metadata()
        .with_context(|| format!("stat external agent binary {}", canonical_path.display()))?;
    let reported_len = meta.len();
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 32 * 1024];
    let mut read_total = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read external agent binary {}", canonical_path.display()))?;
        if read == 0 {
            break;
        }
        read_total = read_total.saturating_add(read as u64);
        if read_total > reported_len {
            bail!(
                "external agent binary yielded more than the {reported_len} bytes it reported \
                 while being hashed; refusing to trust it (it must be a regular file that is \
                 not being modified — stop the writer, then trust it again)"
            );
        }
        hasher.update(&buffer[..read]);
    }
    let sha256 = hex::encode(hasher.finalize());
    #[cfg(unix)]
    let (device, inode, mtime) = {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino(), meta.mtime())
    };
    #[cfg(windows)]
    let (device, inode, mtime) = {
        let (device, inode) = crate::utils::windows_fs::identity(&file)
            .context("inspect Windows external agent binary identity")?;
        let mtime = meta
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as i64;
        (device, inode, mtime)
    };
    #[cfg(not(any(unix, windows)))]
    let (device, inode, mtime) = {
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        (0u64, 0u64, mtime)
    };
    Ok(Provenance {
        canonical_path,
        sha256,
        device,
        inode,
        mtime,
    })
}

/// Best-effort spawn-surface hardening: the binary's parent directory must
/// not be world-writable (a world-writable dir lets any local user swap the
/// verified binary between check and exec).
pub fn ensure_parent_not_world_writable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("binary {} has no parent directory", path.display()))?;
        let meta = std::fs::metadata(parent)
            .with_context(|| format!("stat parent directory {}", parent.display()))?;
        if meta.permissions().mode() & 0o002 != 0 {
            bail!(
                "parent directory {} of the external agent binary is world-writable; \
                 refusing to spawn (move the binary to a protected directory)",
                parent.display()
            );
        }
    }
    #[cfg(windows)]
    {
        let parent = path
            .parent()
            .context("trusted Windows binary has no parent directory")?;
        crate::utils::windows_fs::ensure_directory_not_publicly_writable(parent)
            .context("validate trusted Windows binary parent ACL")?;
    }
    #[cfg(not(any(unix, windows)))]
    let _ = path;
    Ok(())
}

/// A0-08: the directory itself (not just its parent) must not be
/// world-writable — a world-writable trusted dir lets any local user drop a
/// binary that would then be trustable.
pub fn ensure_dir_not_world_writable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path)
            .with_context(|| format!("stat directory {}", path.display()))?;
        if !meta.is_dir() {
            bail!("{} is not a directory", path.display());
        }
        if meta.permissions().mode() & 0o002 != 0 {
            bail!(
                "directory {} is world-writable; refusing to trust it (use a protected directory)",
                path.display()
            );
        }
    }
    #[cfg(windows)]
    crate::utils::windows_fs::ensure_directory_not_publicly_writable(path)
        .context("validate trusted Windows directory ACL")?;
    #[cfg(not(any(unix, windows)))]
    let _ = path;
    Ok(())
}

/// Expand a single leading `~` against `$HOME` (best-effort; returns the path
/// unchanged when there is no HOME or no leading `~`).
fn expand_tilde(raw: &str) -> PathBuf {
    #[cfg(windows)]
    if let Some(home) = dirs::home_dir() {
        if raw == "~" {
            return home;
        }
        if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
            return home.join(rest);
        }
    }
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return Path::new(&home).join(rest);
    }
    if raw == "~"
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home);
    }
    PathBuf::from(raw)
}

/// A0-08: the operator-configured trusted directory allowlist, tilde-expanded.
/// Absent config yields [`DEFAULT_TRUSTED_DIRS`]; a corrupt value fails closed
/// to the default rather than trusting everything.
pub async fn read_trusted_dirs() -> Result<Vec<PathBuf>> {
    let entry = ConfigKv::get(TRUSTED_DIRS_KEY)
        .await
        .context("read agent.external_agents.trusted_dirs")?;
    Ok(decode_trusted_dirs(entry.map(|entry| entry.value)))
}

fn decode_trusted_dirs(value: Option<String>) -> Vec<PathBuf> {
    let raw: Vec<String> = match value {
        Some(value) => serde_json::from_str(&value)
            .unwrap_or_else(|_| DEFAULT_TRUSTED_DIRS.iter().map(|s| s.to_string()).collect()),
        None => DEFAULT_TRUSTED_DIRS.iter().map(|s| s.to_string()).collect(),
    };
    raw.iter().map(|s| expand_tilde(s)).collect()
}

/// Read on the caller's connection, including inside a writer transaction.
/// This port preserves the existing directory grammar and fallback without
/// acquiring another pooled connection while the caller holds a write lock.
pub async fn read_trusted_dirs_with_conn<C: ConnectionTrait>(db: &C) -> Result<Vec<PathBuf>> {
    let entry = ConfigKv::get_with_conn(db, TRUSTED_DIRS_KEY)
        .await
        .context("read agent.external_agents.trusted_dirs on registration connection")?;
    Ok(decode_trusted_dirs(entry.map(|entry| entry.value)))
}

/// Backend-agnostic counterpart of [`read_trusted_dirs_with_conn`] using [`crate::storage::ConfigStore`].
pub async fn read_trusted_dirs_with_store(
    store: &dyn crate::storage::ConfigStore,
) -> Result<Vec<PathBuf>> {
    let entry = ConfigKv::get_with_store(store, TRUSTED_DIRS_KEY)
        .await
        .context("read agent.external_agents.trusted_dirs from store")?;
    Ok(decode_trusted_dirs(entry.map(|entry| entry.value)))
}

/// Backend-agnostic counterpart of [`add_trusted_dir`] using [`crate::storage::ConfigStore`].
pub async fn add_trusted_dir_with_store(
    store: &dyn crate::storage::ConfigStore,
    path: &Path,
) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("canonicalize trusted directory {}", path.display()))?;
    ensure_dir_not_world_writable(&canonical)?;

    let entry = ConfigKv::get_with_store(store, TRUSTED_DIRS_KEY)
        .await
        .context("read agent.external_agents.trusted_dirs from store")?;
    let mut raw: Vec<String> = match entry {
        Some(entry) => serde_json::from_str(&entry.value).unwrap_or_default(),
        None => DEFAULT_TRUSTED_DIRS.iter().map(|s| s.to_string()).collect(),
    };
    let canonical_str = canonical.to_string_lossy().to_string();
    if !raw.iter().any(|d| expand_tilde(d) == canonical) {
        raw.push(canonical_str);
        let value = serde_json::to_string(&raw).context("serialize trusted_dirs")?;
        ConfigKv::set_with_store(store, TRUSTED_DIRS_KEY, &value, false)
            .await
            .context("persist agent.external_agents.trusted_dirs in store")?;
    }
    Ok(canonical)
}

/// A0-08: append `path` to the trusted-directory allowlist. The path is
/// canonicalized and must be an existing, non-world-writable directory.
/// Idempotent — a directory already present is not duplicated. Returns the
/// canonical path recorded.
pub async fn add_trusted_dir(path: &Path) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("canonicalize trusted directory {}", path.display()))?;
    ensure_dir_not_world_writable(&canonical)?;

    // Read the raw (un-expanded) list so re-runs stay stable, then append the
    // canonical string form if absent.
    let entry = ConfigKv::get(TRUSTED_DIRS_KEY)
        .await
        .context("read agent.external_agents.trusted_dirs")?;
    let mut raw: Vec<String> = match entry {
        Some(entry) => serde_json::from_str(&entry.value).unwrap_or_default(),
        None => DEFAULT_TRUSTED_DIRS.iter().map(|s| s.to_string()).collect(),
    };
    let canonical_str = canonical.to_string_lossy().to_string();
    if !raw.iter().any(|d| expand_tilde(d) == canonical) {
        raw.push(canonical_str);
        let value = serde_json::to_string(&raw).context("serialize trusted_dirs")?;
        ConfigKv::set(TRUSTED_DIRS_KEY, &value, false)
            .await
            .context("persist agent.external_agents.trusted_dirs")?;
    }
    Ok(canonical)
}

/// Trust-record slug for the OpenCode provider-exporter binary (DR-04b).
pub const OPENCODE_EXPORTER_SLUG: &str = "opencode";

/// Trust-record slug for the Hermes CLI exporter (HM-08).
///
/// This record is only the Hermes executable. It does not authorize OpenCode
/// and OpenCode trust does not authorize Hermes.
pub const HERMES_EXPORTER_SLUG: &str = "hermes";

/// Provider-exporter slugs: built-in providers whose OWN CLI binary may be
/// trusted (under `agent.trust.<slug>`) to power a sandboxed export bridge.
/// These are deliberately NOT `libra-agent-*` RPC binaries — `agent rpc
/// trust <slug>` recognizes them and pins the provider binary found under a
/// registered trusted directory instead of scanning `$PATH`, and the
/// built-in-slug impersonation guard (`LBR-AGENT-006`) does not apply
/// because the record is FOR the built-in provider itself.
pub const PROVIDER_EXPORTER_TRUST_SLUGS: &[&str] = &[OPENCODE_EXPORTER_SLUG, HERMES_EXPORTER_SLUG];

/// Whether `slug` names a built-in provider exporter (see
/// [`PROVIDER_EXPORTER_TRUST_SLUGS`]).
pub fn is_provider_exporter_slug(slug: &str) -> bool {
    PROVIDER_EXPORTER_TRUST_SLUGS.contains(&slug)
}

/// Bytes inspected to recognise a package-manager launcher. Real shims are a
/// few KiB at most (pnpm's embeds absolute `NODE_PATH` lists and writes its
/// target marker last); a native executable is recognised by its first bytes.
const LAUNCHER_PROBE_BYTES: u64 = 64 * 1024;

/// How a provider-exporter file behaves once the export sandbox maps only its
/// own bytes at a fixed private path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExporterLauncher {
    /// The file runs by itself: a native executable or a self-contained
    /// script.
    Direct,
    /// A package-manager shim that declares the file it executes: mise's npm
    /// backend (`# aube-bin-shim v2 target=<relative path>`) or pnpm's
    /// cmd-shim (`# cmd-shim-target=<absolute path>`). The shim locates that
    /// target relative to its own host location, which does not exist inside
    /// the sandbox, so only the target itself can be pinned.
    Shim { target: PathBuf },
    /// A script run by a JavaScript runtime (`node`, `bun`, `deno`) that
    /// locates the provider binary in its package tree at run time.
    JavaScript,
}

/// Classify the provider exporter at `path` (followed through symlinks) from
/// its leading bytes. Nothing is executed; a declared shim target is returned
/// unresolved and unverified.
pub fn classify_exporter_launcher(path: &Path) -> Result<ExporterLauncher> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("canonicalize provider exporter {}", path.display()))?;
    let mut head = Vec::new();
    fs::File::open(&canonical)
        .and_then(|file| file.take(LAUNCHER_PROBE_BYTES).read_to_end(&mut head))
        .with_context(|| format!("read provider exporter {}", canonical.display()))?;
    let complete = (head.len() as u64) < LAUNCHER_PROBE_BYTES;
    Ok(classify_launcher_bytes(&head, complete, canonical.parent()))
}

/// Pure core of [`classify_exporter_launcher`]. `complete` is false when
/// `head` may end inside a line, whose partial text is then ignored.
/// Relative shim targets resolve against `shim_dir`, the shim's canonical
/// directory — where its own `$0` symlink walk ends.
fn classify_launcher_bytes(
    head: &[u8],
    complete: bool,
    shim_dir: Option<&Path>,
) -> ExporterLauncher {
    let Some(script) = head.strip_prefix(b"#!") else {
        return ExporterLauncher::Direct;
    };
    let script = if complete {
        script
    } else {
        match script.iter().rposition(|byte| *byte == b'\n') {
            Some(end) => &script[..end],
            None => &[],
        }
    };
    let text = String::from_utf8_lossy(script);
    let mut lines = text.lines();
    if lines.next().is_some_and(shebang_runs_javascript) {
        return ExporterLauncher::JavaScript;
    }
    for line in lines {
        let line = line.trim();
        let declared = match line.strip_prefix("# aube-bin-shim ") {
            Some(header) => header.split_once("target=").map(|(_, target)| target),
            None => line.strip_prefix("# cmd-shim-target="),
        };
        let Some(declared) = declared.map(str::trim).filter(|target| !target.is_empty()) else {
            continue;
        };
        let declared = Path::new(declared);
        let target = match shim_dir {
            Some(dir) if declared.is_relative() => dir.join(declared),
            _ => declared.to_path_buf(),
        };
        return ExporterLauncher::Shim { target };
    }
    ExporterLauncher::Direct
}

/// Whether a `#!` line hands its script to a JavaScript runtime, directly
/// (`#!/usr/bin/node`) or through `env` (`#!/usr/bin/env node`,
/// `#!/usr/bin/env -S bun --smol`).
fn shebang_runs_javascript(shebang: &str) -> bool {
    fn program_name(word: &str) -> &str {
        word.rsplit('/').next().unwrap_or(word)
    }
    let mut words = shebang.split_whitespace();
    let Some(mut program) = words.next().map(program_name) else {
        return false;
    };
    if program == "env" {
        // Skip env's own options and NAME=value assignments.
        let Some(next) = words.find(|word| !word.starts_with('-') && !word.contains('=')) else {
            return false;
        };
        program = program_name(next);
    }
    matches!(program, "node" | "nodejs" | "bun" | "deno")
}

/// Whether `head` starts with a native executable image: ELF, Mach-O (thin
/// or universal) or PE.
fn is_native_executable_header(head: &[u8]) -> bool {
    const MAGICS: [&[u8]; 8] = [
        b"\x7fELF",
        &[0xfe, 0xed, 0xfa, 0xce],
        &[0xce, 0xfa, 0xed, 0xfe],
        &[0xfe, 0xed, 0xfa, 0xcf],
        &[0xcf, 0xfa, 0xed, 0xfe],
        &[0xca, 0xfe, 0xba, 0xbe],
        &[0xbe, 0xba, 0xfe, 0xca],
        b"MZ",
    ];
    MAGICS.iter().any(|magic| head.starts_with(magic))
}

/// A provider exporter located under a trusted directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedExporter {
    /// The file to trust and pin: a directly runnable exporter, or the
    /// canonical native executable a package-manager shim launches.
    pub binary: PathBuf,
    /// The shim that named `binary`, when one was followed.
    pub launcher: Option<PathBuf>,
}

/// DR-04b: locate the provider-exporter binary named `slug` by probing the
/// registered trusted directories — never `$PATH`, so exporter trust cannot
/// widen the gated external-binary discovery surface. The first
/// `<trusted-dir>/<slug>` regular file that the export sandbox can run wins.
/// A recognised package-manager shim is followed one hop to the native
/// executable it declares, which must itself lie under a trusted directory;
/// a JavaScript launcher is refused. `record_trust` afterwards re-enforces
/// canonicalization, world-writable refusal and trusted-directory
/// containment on the final target.
pub async fn resolve_exporter_binary(slug: &str) -> Result<ResolvedExporter> {
    let dirs = read_trusted_dirs().await?;
    let mut refusals = Vec::new();
    for dir in &dirs {
        #[cfg(windows)]
        let candidate = dir.join(format!("{slug}.exe"));
        #[cfg(not(windows))]
        let candidate = dir.join(slug);
        if !fs::metadata(&candidate)
            .map(|m| m.is_file())
            .unwrap_or(false)
        {
            continue;
        }
        match resolve_exporter_candidate(slug, &candidate, &dirs) {
            Ok(resolved) => return Ok(resolved),
            Err(refusal) => refusals.push(format!("{refusal:#}")),
        }
    }
    if !refusals.is_empty() {
        bail!("{}", refusals.join("; "));
    }
    bail!(
        "no '{slug}' binary found under a trusted directory; register the directory containing \
         the verified binary first with `libra agent rpc trust --dir <path>` (trusted dirs: {})",
        dirs.iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Map one `<trusted-dir>/<slug>` candidate to the file the export bridge
/// can run (see [`resolve_exporter_binary`]).
fn resolve_exporter_candidate(
    slug: &str,
    candidate: &Path,
    trusted_dirs: &[PathBuf],
) -> Result<ResolvedExporter> {
    let target = match classify_exporter_launcher(candidate)? {
        ExporterLauncher::Direct => {
            return Ok(ResolvedExporter {
                binary: candidate.to_path_buf(),
                launcher: None,
            });
        }
        ExporterLauncher::JavaScript => bail!(
            "{} is a JavaScript launcher that the export sandbox cannot run; register the \
             directory of the native '{slug}' executable with `libra agent rpc trust --dir <path>` \
             and re-run `libra agent rpc trust {slug}`",
            candidate.display()
        ),
        ExporterLauncher::Shim { target } => target,
    };
    let binary = target.canonicalize().with_context(|| {
        format!(
            "{} is a package-manager launcher for {}, which does not exist; reinstall '{slug}' or \
             register the directory of its native executable with `libra agent rpc trust --dir <path>`",
            candidate.display(),
            target.display()
        )
    })?;
    let mut head = Vec::new();
    let native = fs::metadata(&binary).is_ok_and(|meta| meta.is_file())
        && fs::File::open(&binary)
            .and_then(|file| file.take(4).read_to_end(&mut head))
            .is_ok()
        && is_native_executable_header(&head);
    if !native {
        bail!(
            "{} is a package-manager launcher for {}, which is not a native executable the export \
             sandbox can run; register the directory of the native '{slug}' executable with \
             `libra agent rpc trust --dir <path>` and re-run `libra agent rpc trust {slug}`",
            candidate.display(),
            binary.display()
        );
    }
    if !path_within_trusted_dirs(&binary, trusted_dirs) {
        let directory = binary.parent().unwrap_or(&binary);
        bail!(
            "{} is a package-manager launcher for {}; the export sandbox runs only that \
             executable, which is not under a trusted directory — register it with \
             `libra agent rpc trust --dir {}` and re-run `libra agent rpc trust {slug}`",
            candidate.display(),
            binary.display(),
            directory.display()
        );
    }
    Ok(ResolvedExporter {
        binary,
        launcher: Some(candidate.to_path_buf()),
    })
}

/// A0-08: whether `canonical_binary` lives under one of the trusted `dirs`.
/// Both sides are assumed already canonicalized.
pub fn path_within_trusted_dirs(canonical_binary: &Path, dirs: &[PathBuf]) -> bool {
    dirs.iter().any(|dir| {
        dir.canonicalize()
            .map(|d| canonical_binary.starts_with(&d))
            .unwrap_or_else(|_| canonical_binary.starts_with(dir))
    })
}

/// A0-08: env-var names that must NEVER be passed through to a spawned
/// external agent, regardless of `env_allowlist_extra`. Rejects wildcards and
/// any credential/endpoint name (matched case-insensitively, ASCII).
pub fn env_name_is_forbidden(name: &str) -> bool {
    // A real env name is `[A-Za-z_][A-Za-z0-9_]*`; anything else (notably a
    // literal `*` wildcard) is refused outright.
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return true;
    }
    let upper = name.to_ascii_uppercase();
    const FORBIDDEN_SUFFIXES: &[&str] = &["_API_KEY", "_TOKEN", "_SECRET", "_PASSWORD"];
    const FORBIDDEN_PREFIXES: &[&str] = &["LIBRA_STORAGE_", "LIBRA_D1_"];
    FORBIDDEN_SUFFIXES.iter().any(|s| upper.ends_with(s))
        || FORBIDDEN_PREFIXES.iter().any(|p| upper.starts_with(p))
}

/// A0-08: the operator-configured extra env-passthrough names, with any
/// forbidden name silently dropped (defense-in-depth — the spawn path also
/// re-checks). Absent/corrupt config yields an empty list.
pub async fn env_allowlist_extra() -> Result<Vec<String>> {
    let entry = ConfigKv::get(ENV_ALLOWLIST_EXTRA_KEY)
        .await
        .context("read agent.external_agents.env_allowlist_extra")?;
    let raw: Vec<String> = match entry {
        Some(entry) => serde_json::from_str(&entry.value).unwrap_or_default(),
        None => Vec::new(),
    };
    Ok(raw
        .into_iter()
        .filter(|name| !env_name_is_forbidden(name))
        .collect())
}

fn trust_key(slug: &str) -> String {
    format!("{TRUST_KEY_PREFIX}{slug}")
}

/// Record trust for `slug` at `path`, replacing any previous record.
///
/// Fails closed (nothing is persisted) when the binary's parent directory
/// is world-writable: trusting such a binary would be meaningless because
/// any local user could swap it before the invoke-time revalidation.
pub async fn record_trust(slug: &str, path: &Path) -> Result<TrustRecord> {
    let provenance = compute_provenance(path)?;
    ensure_parent_not_world_writable(&provenance.canonical_path)?;
    // A0-08: the binary must live under a trusted directory — an arbitrary
    // user-writable $PATH entry is not trustable even if its parent happens
    // not to be world-writable.
    let dirs = read_trusted_dirs().await?;
    if !path_within_trusted_dirs(&provenance.canonical_path, &dirs) {
        bail!(
            "external agent binary {} is not under a trusted directory; \
             register its directory first with `libra agent rpc trust --dir <path>` \
             (trusted dirs: {})",
            provenance.canonical_path.display(),
            dirs.iter()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let record = TrustRecord {
        path: provenance.canonical_path.clone(),
        sha256: provenance.sha256,
        device: provenance.device,
        inode: provenance.inode,
        mtime: provenance.mtime,
    };
    let value = serde_json::to_string(&record).context("serialize trust record")?;
    ConfigKv::set(&trust_key(slug), &value, false)
        .await
        .with_context(|| format!("persist trust record for '{slug}'"))?;
    Ok(record)
}

/// Read the trust record for `slug`, if any.
pub async fn read_trust(slug: &str) -> Result<Option<TrustRecord>> {
    let Some(entry) = ConfigKv::get(&trust_key(slug))
        .await
        .with_context(|| format!("read trust record for '{slug}'"))?
    else {
        return Ok(None);
    };
    let record: TrustRecord = serde_json::from_str(&entry.value)
        .with_context(|| format!("parse trust record for '{slug}' (corrupt config value)"))?;
    Ok(Some(record))
}

/// Read the existing trust owner on the caller's connection. Registration
/// must use this after acquiring its writer lock, rather than a pooled read.
pub async fn read_trust_with_conn<C: ConnectionTrait>(
    db: &C,
    slug: &str,
) -> Result<Option<TrustRecord>> {
    let Some(entry) = ConfigKv::get_with_conn(db, &trust_key(slug))
        .await
        .with_context(|| format!("read trust record for '{slug}' on registration connection"))?
    else {
        return Ok(None);
    };
    if entry.encrypted {
        bail!("trust record for '{slug}' is encrypted; restore a valid exporter trust record");
    }
    serde_json::from_str(&entry.value)
        .map_err(|_| anyhow::anyhow!("trust record for '{slug}' is corrupt; restore a valid exporter trust record before registration"))
        .map(Some)
}

/// Backend-agnostic counterpart of [`record_trust`] using [`crate::storage::ConfigStore`].
pub async fn record_trust_with_store(
    store: &dyn crate::storage::ConfigStore,
    slug: &str,
    path: &Path,
) -> Result<TrustRecord> {
    let provenance = compute_provenance(path)?;
    ensure_parent_not_world_writable(&provenance.canonical_path)?;
    let dirs = read_trusted_dirs_with_store(store).await?;
    if !path_within_trusted_dirs(&provenance.canonical_path, &dirs) {
        bail!(
            "external agent binary {} is not under a trusted directory; \
             register its directory first with `libra agent rpc trust --dir <path>` \
             (trusted dirs: {})",
            provenance.canonical_path.display(),
            dirs.iter()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let record = TrustRecord {
        path: provenance.canonical_path.clone(),
        sha256: provenance.sha256,
        device: provenance.device,
        inode: provenance.inode,
        mtime: provenance.mtime,
    };
    let value = serde_json::to_string(&record).context("serialize trust record")?;
    ConfigKv::set_with_store(store, &trust_key(slug), &value, false)
        .await
        .with_context(|| format!("persist trust record for '{slug}' in store"))?;
    Ok(record)
}

/// Backend-agnostic counterpart of [`read_trust_with_conn`] using [`crate::storage::ConfigStore`].
pub async fn read_trust_with_store(
    store: &dyn crate::storage::ConfigStore,
    slug: &str,
) -> Result<Option<TrustRecord>> {
    let Some(entry) = ConfigKv::get_with_store(store, &trust_key(slug))
        .await
        .with_context(|| format!("read trust record for '{slug}' from store"))?
    else {
        return Ok(None);
    };
    if entry.encrypted {
        bail!("trust record for '{slug}' is encrypted; restore a valid exporter trust record");
    }
    serde_json::from_str(&entry.value)
        .map_err(|_| anyhow::anyhow!("trust record for '{slug}' is corrupt; restore a valid exporter trust record before registration"))
        .map(Some)
}

/// Backend-agnostic counterpart of [`revoke_trust_with_conn`] using [`crate::storage::ConfigStore`].
pub async fn revoke_trust_with_store(
    store: &dyn crate::storage::ConfigStore,
    slug: &str,
) -> Result<bool> {
    ConfigKv::unset_with_store(store, &trust_key(slug))
        .await
        .with_context(|| format!("remove trust record for '{slug}' from store"))
        .map(|removed| removed > 0)
}

/// Revoke through the caller's transaction; the native lifecycle owner can
/// atomically revoke its registry without nesting a pooled acquisition.
pub async fn revoke_trust_with_conn<C: ConnectionTrait>(db: &C, slug: &str) -> Result<bool> {
    ConfigKv::unset_with_conn(db, &trust_key(slug))
        .await
        .with_context(|| format!("remove trust record for '{slug}' on registration connection"))
        .map(|removed| removed > 0)
}

/// Remove the trust record for `slug`. Returns whether one existed.
pub async fn revoke_trust(slug: &str) -> Result<bool> {
    let removed = ConfigKv::unset(&trust_key(slug))
        .await
        .with_context(|| format!("remove trust record for '{slug}'"))?;
    Ok(removed > 0)
}

/// Pure drift check between freshly computed provenance and a recorded
/// trust decision: any single marker changing (hash, device, inode,
/// mtime or canonical path) counts as drift.
pub fn provenance_drifted(provenance: &Provenance, record: &TrustRecord) -> bool {
    provenance.sha256 != record.sha256
        || provenance.device != record.device
        || provenance.inode != record.inode
        || provenance.mtime != record.mtime
        || provenance.canonical_path != record.path
}

/// Revalidate the recorded trust for `slug` against the binary on disk,
/// immediately before spawn. Any drift (hash, device, inode, mtime or path)
/// revokes the record and fails closed — the binary returns to quarantine
/// until the operator re-trusts it (E2 / `LBR-AGENT-005`).
///
/// The record and the trusted-directory allowlist are read from the process
/// repository. Callers that already hold another repository connection must
/// use [`revalidate_trust_with_conn`] so a refusal cannot revoke a different
/// checkout.
pub async fn revalidate_trust(slug: &str, record: &TrustRecord) -> Result<Provenance> {
    let storage = crate::utils::util::try_get_storage_path(None)
        .context("revalidate external agent trust requires a Libra repository")?;
    match crate::storage::runtime::detect_repo_backend(&storage)
        .map_err(|err| anyhow::anyhow!("detect repository storage backend: {err}"))?
    {
        crate::storage::StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let store = crate::storage::runtime::open_repo_config_store_read_write(&storage)
                    .await
                    .map_err(|err| {
                        anyhow::anyhow!("open redb config store for trust revalidation: {err}")
                    })?;
                revalidate_trust_with_store(store.as_ref(), slug, record).await
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                bail!("this Libra binary was compiled without `storage-redb`");
            }
        }
        crate::storage::StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let db = get_db_conn_instance().await;
                revalidate_trust_with_conn(&db, slug, record).await
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                bail!("this Libra binary was compiled without `storage-sqlite`");
            }
        }
    }
}

/// Backend-agnostic counterpart of [`revalidate_trust_with_conn`] using [`crate::storage::ConfigStore`].
pub async fn revalidate_trust_with_store(
    store: &dyn crate::storage::ConfigStore,
    slug: &str,
    record: &TrustRecord,
) -> Result<Provenance> {
    let record_path = record.path.clone();
    let provenance =
        match tokio::task::spawn_blocking(move || compute_provenance(&record_path)).await {
            Ok(Ok(provenance)) => provenance,
            Ok(Err(err)) => {
                let _ = revoke_trust_with_store(store, slug).await;
                return Err(err.context(format!(
                    "trusted binary for '{slug}' is no longer readable; trust revoked"
                )));
            }
            Err(err) => {
                let _ = revoke_trust_with_store(store, slug).await;
                return Err(anyhow::Error::new(err).context(format!(
                    "trusted binary for '{slug}' could not complete revalidation; trust revoked"
                )));
            }
        };
    if provenance_drifted(&provenance, record) {
        let _ = revoke_trust_with_store(store, slug).await;
        bail!(
            "external agent binary for '{slug}' changed since it was trusted \
             (sha256/device/inode/mtime drift); trust revoked — re-run \
             'libra agent rpc trust {slug}' after verifying the binary"
        );
    }
    let dirs = read_trusted_dirs_with_store(store).await?;
    if !path_within_trusted_dirs(&provenance.canonical_path, &dirs) {
        let _ = revoke_trust_with_store(store, slug).await;
        bail!(
            "external agent binary for '{slug}' is no longer under a trusted directory; \
             trust revoked — re-register its directory with \
             'libra agent rpc trust --dir <path>' then re-trust it"
        );
    }
    Ok(provenance)
}

/// [`revalidate_trust`] on the caller's connection.
///
/// Hashing stays off the runtime. Revocation and the directory allowlist use
/// `db`, which is the connection that stored the trust record.
pub async fn revalidate_trust_with_conn<C: ConnectionTrait>(
    db: &C,
    slug: &str,
    record: &TrustRecord,
) -> Result<Provenance> {
    // Hashing/stat-ing an operator-selected executable may block on a slow
    // filesystem (including FUSE). Keep that synchronous work off Tokio's
    // executor; callers with a stricter operation deadline can cancel their
    // await without ever reaching a spawn boundary. A detached blocking task
    // owns only local file handles and drops them when it eventually returns.
    let record_path = record.path.clone();
    let provenance =
        match tokio::task::spawn_blocking(move || compute_provenance(&record_path)).await {
            Ok(Ok(provenance)) => provenance,
            Ok(Err(err)) => {
                let _ = revoke_trust_with_conn(db, slug).await;
                return Err(err.context(format!(
                    "trusted binary for '{slug}' is no longer readable; trust revoked"
                )));
            }
            Err(err) => {
                let _ = revoke_trust_with_conn(db, slug).await;
                return Err(anyhow::Error::new(err).context(format!(
                    "trusted binary for '{slug}' could not complete revalidation; trust revoked"
                )));
            }
        };
    if provenance_drifted(&provenance, record) {
        let _ = revoke_trust_with_conn(db, slug).await;
        bail!(
            "external agent binary for '{slug}' changed since it was trusted \
             (sha256/device/inode/mtime drift); trust revoked — re-run \
             'libra agent rpc trust {slug}' after verifying the binary"
        );
    }
    // A0-08 defense-in-depth: if the binary's directory was removed from the
    // trusted allowlist since it was trusted, drop it back to quarantine.
    let dirs = read_trusted_dirs_with_conn(db).await?;
    if !path_within_trusted_dirs(&provenance.canonical_path, &dirs) {
        let _ = revoke_trust_with_conn(db, slug).await;
        bail!(
            "external agent binary for '{slug}' is no longer under a trusted directory; \
             trust revoked — re-register its directory with \
             'libra agent rpc trust --dir <path>' then re-trust it"
        );
    }
    Ok(provenance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_provenance_hashes_and_stats() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("libra-agent-demo");
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        let p = compute_provenance(&path).unwrap();
        assert_eq!(p.sha256.len(), 64);
        assert!(p.canonical_path.is_absolute());
        #[cfg(unix)]
        {
            assert_ne!(p.inode, 0);
        }
    }

    /// R85: a realistic single-file provider CLI must stay trustable. The
    /// stock OpenCode CLI is a ~171 MiB Bun binary, so a 200 MiB (sparse)
    /// exporter is hashed, matches the whole-file sha256 that earlier
    /// releases recorded, and revalidates without drift. Any size cap below
    /// real binaries (the former 64 MiB one refused and revoked OpenCode
    /// trust) fails this test.
    #[test]
    fn compute_provenance_hashes_large_single_file_exporter_without_size_cap() {
        const LARGE_EXPORTER_BYTES: u64 = 200 * 1024 * 1024;
        // `printf '#!/bin/sh\nexit 0\n'` zero-extended to 200 MiB, digested
        // independently with `shasum -a 256`.
        const LARGE_EXPORTER_SHA256: &str =
            "4e045ec6b0ef4cdc8c82e9d87983f6bafb0e2950c0c8ad971d391657589ec0c4";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode");
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|file| file.set_len(LARGE_EXPORTER_BYTES))
            .expect("extend exporter sparsely past the real OpenCode size");

        let trusted = compute_provenance(&path).expect("large exporter must be trustable");
        assert_eq!(trusted.sha256, LARGE_EXPORTER_SHA256);
        let revalidated = compute_provenance(&path).expect("large exporter must revalidate");
        assert!(
            !provenance_drifted(&revalidated, &record_from(&trusted)),
            "re-hashing an unchanged large exporter must not drift"
        );
    }

    /// Without a size cap the read is still bounded by the descriptor's own
    /// stat length: a stream that keeps producing bytes past it fails closed
    /// promptly instead of being hashed without end.
    #[cfg(unix)]
    #[test]
    fn compute_provenance_refuses_stream_longer_than_its_stat_length() {
        let error = compute_provenance(Path::new("/dev/zero"))
            .expect_err("an endless device stream must not be trusted");
        assert!(
            error
                .to_string()
                .contains("more than the 0 bytes it reported"),
            "unexpected endless-stream error: {error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn world_writable_parent_is_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("libra-agent-demo");
        std::fs::write(&path, b"x").unwrap();
        ensure_parent_not_world_writable(&path).expect("0700 tempdir parent is fine");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let err = ensure_parent_not_world_writable(&path).unwrap_err();
        assert!(err.to_string().contains("world-writable"));
    }

    fn record_from(p: &Provenance) -> TrustRecord {
        TrustRecord {
            path: p.canonical_path.clone(),
            sha256: p.sha256.clone(),
            device: p.device,
            inode: p.inode,
            mtime: p.mtime,
        }
    }

    /// Inode-only drift (same bytes, different inode) must count as
    /// drift — content equality is not enough to keep trust. The drift
    /// is synthesized on the record rather than via remove+recreate:
    /// filesystems like ext4 routinely reuse a just-freed inode, so a
    /// recreate is NOT guaranteed to change it.
    #[cfg(unix)]
    #[test]
    fn inode_drift_with_identical_content_is_drift() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("libra-agent-demo");
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        let fresh = compute_provenance(&path).unwrap();
        let mut record = record_from(&fresh);
        assert!(!provenance_drifted(&fresh, &record));
        record.inode = record.inode.wrapping_add(1);
        assert_eq!(fresh.sha256, record.sha256, "content markers identical");
        assert!(
            provenance_drifted(&fresh, &record),
            "inode-only change must count as drift"
        );
    }

    /// The `.bin/opencode` shim written by mise's npm backend (aube) for the
    /// `@opencode/cli` package, verbatim apart from the target.
    fn aube_shim(target: &str) -> String {
        format!(
            "#!/bin/sh\n# aube-bin-shim v2 target={target}\nlink=\"$0\"\nhops=0\n\
             while [ -L \"$link\" ] && [ \"$hops\" -lt 40 ]; do\nhops=$((hops+1))\n\
             target=$(readlink \"$link\")\ncase \"$target\" in\n/*) link=\"$target\" ;;\n\
             *)  link=\"$(dirname \"$link\")/$target\" ;;\nesac\ndone\n\
             basedir=$(dirname \"$link\")\n\
             export NODE_PATH=\"$basedir/..:$basedir/../.mise/node_modules\"\n\
             exec \"$basedir/{target}\" \"$@\"\n"
        )
    }

    /// pnpm's cmd-shim for a native package binary: a `$basedir`-relative
    /// exec plus the absolute target marker as its last line.
    fn pnpm_shim(target: &Path) -> String {
        format!(
            "#!/bin/sh\nbasedir=$(dirname \"$(echo \"$0\" | sed -e 's,\\\\,/,g')\")\n\n\
             exec \"$basedir/../.pnpm/opencode/bin/opencode.exe\"   \"$@\"\nexit $?\n\
             # cmd-shim-target={}\n",
            target.display()
        )
    }

    #[cfg(unix)]
    fn write_executable(path: &Path, bytes: &[u8]) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn launcher_classification_recognises_only_declared_shims_and_js_runtimes() {
        let shim_dir = Path::new("/opt/tool/node_modules/.bin");
        let classify = |bytes: &[u8]| classify_launcher_bytes(bytes, true, Some(shim_dir));

        assert_eq!(classify(b"\x7fELF\x02\x01\x01"), ExporterLauncher::Direct);
        assert_eq!(
            classify(&[0xcf, 0xfa, 0xed, 0xfe, 0x07]),
            ExporterLauncher::Direct
        );
        // Self-contained scripts keep running from their own bytes.
        assert_eq!(classify(b"#!/bin/sh\nexit 0\n"), ExporterLauncher::Direct);
        assert_eq!(
            classify(b"#!/bin/sh\nprintf '{\"info\":{}}'\n"),
            ExporterLauncher::Direct
        );

        let relative = "../.mise/@opencode+cli@2.0.29/node_modules/@opencode/cli/bin/opencode.exe";
        assert_eq!(
            classify(aube_shim(relative).as_bytes()),
            ExporterLauncher::Shim {
                target: shim_dir.join(relative)
            }
        );
        let absolute = Path::new("/opt/tool/node_modules/.pnpm/opencode/bin/opencode.exe");
        assert_eq!(
            classify(pnpm_shim(absolute).as_bytes()),
            ExporterLauncher::Shim {
                target: absolute.to_path_buf()
            }
        );

        for shebang in [
            "#!/usr/bin/env node\nrequire('./launch')\n",
            "#!/usr/bin/env -S bun --smol\n",
            "#!/usr/bin/env NODE_OPTIONS=--no-warnings nodejs\n",
            "#!/usr/local/bin/deno run\n",
            "#!/usr/bin/node\r\n",
        ] {
            assert_eq!(
                classify(shebang.as_bytes()),
                ExporterLauncher::JavaScript,
                "{shebang:?}"
            );
        }
        assert_eq!(classify(b"#!/usr/bin/env\n"), ExporterLauncher::Direct);
    }

    /// A probe that stopped mid-file must not act on a truncated marker line.
    #[test]
    fn launcher_classification_ignores_a_partial_trailing_line() {
        let head = b"#!/bin/sh\nexit 0\n# cmd-shim-target=/opt/tool/bin/openc";
        assert_eq!(
            classify_launcher_bytes(head, false, None),
            ExporterLauncher::Direct
        );
        assert_eq!(
            classify_launcher_bytes(head, true, None),
            ExporterLauncher::Shim {
                target: PathBuf::from("/opt/tool/bin/openc")
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn exporter_candidate_follows_a_shim_to_its_trusted_native_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let bin = root.join("node_modules/.bin");
        let package_bin = root.join("node_modules/.mise/@opencode+cli/bin");
        let native = package_bin.join("opencode.exe");
        write_executable(&native, b"\x7fELF\x02\x01\x01\x00");
        let shim = bin.join("opencode");
        write_executable(
            &shim,
            aube_shim("../.mise/@opencode+cli/bin/opencode.exe").as_bytes(),
        );

        // Only the shim's directory is trusted: the refusal names the exact
        // directory to register next.
        let error = resolve_exporter_candidate("opencode", &shim, std::slice::from_ref(&bin))
            .expect_err("a target outside every trusted directory must be refused");
        let message = format!("{error:#}");
        assert!(
            message.contains("package-manager launcher")
                && message.contains(&format!("trust --dir {}", package_bin.display())),
            "{message}"
        );

        let resolved = resolve_exporter_candidate("opencode", &shim, &[bin.clone(), package_bin])
            .expect("a trusted native target is pinned instead of its shim");
        assert_eq!(
            resolved,
            ResolvedExporter {
                binary: native,
                launcher: Some(shim),
            }
        );

        let pnpm = bin.join("opencode-pnpm");
        let pnpm_native = root.join("node_modules/.pnpm/opencode/bin/opencode.exe");
        write_executable(&pnpm_native, &[0xcf, 0xfa, 0xed, 0xfe, 0x07]);
        write_executable(&pnpm, pnpm_shim(&pnpm_native).as_bytes());
        let resolved = resolve_exporter_candidate("opencode", &pnpm, std::slice::from_ref(&root))
            .expect("pnpm's absolute target marker resolves too");
        assert_eq!(resolved.binary, pnpm_native);
        assert_eq!(resolved.launcher, Some(pnpm));
    }

    #[cfg(unix)]
    #[test]
    fn exporter_candidate_refuses_launchers_the_sandbox_cannot_run() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let trusted = std::slice::from_ref(&root);

        let direct = root.join("direct/opencode");
        write_executable(&direct, b"#!/bin/sh\nexit 0\n");
        assert_eq!(
            resolve_exporter_candidate("opencode", &direct, trusted).unwrap(),
            ResolvedExporter {
                binary: direct.clone(),
                launcher: None,
            }
        );

        let javascript = root.join("js/opencode");
        write_executable(&javascript, b"#!/usr/bin/env node\nrequire('../lib')\n");
        let message = format!(
            "{:#}",
            resolve_exporter_candidate("opencode", &javascript, trusted).unwrap_err()
        );
        assert!(message.contains("JavaScript launcher"), "{message}");

        let missing = root.join("missing/opencode");
        write_executable(&missing, aube_shim("../pkg/bin/opencode.exe").as_bytes());
        let message = format!(
            "{:#}",
            resolve_exporter_candidate("opencode", &missing, trusted).unwrap_err()
        );
        assert!(message.contains("which does not exist"), "{message}");

        // A shim whose target is itself a script (here a JS entry point) is
        // not followed any further.
        let script_target = root.join("script/pkg/bin/opencode.js");
        write_executable(&script_target, b"#!/usr/bin/env node\n");
        let chained = root.join("script/.bin/opencode");
        write_executable(&chained, aube_shim("../pkg/bin/opencode.js").as_bytes());
        let message = format!(
            "{:#}",
            resolve_exporter_candidate("opencode", &chained, trusted).unwrap_err()
        );
        assert!(message.contains("not a native executable"), "{message}");
    }

    /// mtime-only drift (identical bytes and inode, touched timestamp)
    /// must count as drift too — a swapped-back binary shows up as an
    /// mtime change even when hash and inode match again.
    #[cfg(unix)]
    #[test]
    fn mtime_only_drift_is_drift() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("libra-agent-demo");
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        let provenance = compute_provenance(&path).unwrap();
        let mut record = record_from(&provenance);
        assert!(
            !provenance_drifted(&provenance, &record),
            "identical markers must not drift"
        );
        record.mtime -= 1; // recorded one second earlier than on-disk state
        assert!(provenance_drifted(&provenance, &record));
        let mut device_record = record_from(&provenance);
        device_record.device = device_record.device.wrapping_add(1);
        assert!(provenance_drifted(&provenance, &device_record));
    }
}
