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

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::internal::config::ConfigKv;

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
pub fn compute_provenance(path: &Path) -> Result<Provenance> {
    let canonical_path = path
        .canonicalize()
        .with_context(|| format!("canonicalize external agent binary {}", path.display()))?;
    let bytes = std::fs::read(&canonical_path)
        .with_context(|| format!("read external agent binary {}", canonical_path.display()))?;
    let sha256 = hex::encode(Sha256::digest(&bytes));
    let meta = std::fs::metadata(&canonical_path)
        .with_context(|| format!("stat external agent binary {}", canonical_path.display()))?;
    #[cfg(unix)]
    let (device, inode, mtime) = {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino(), meta.mtime())
    };
    #[cfg(not(unix))]
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
    Ok(())
}

/// Expand a single leading `~` against `$HOME` (best-effort; returns the path
/// unchanged when there is no HOME or no leading `~`).
fn expand_tilde(raw: &str) -> PathBuf {
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
    let raw: Vec<String> = match entry {
        Some(entry) => serde_json::from_str(&entry.value)
            .unwrap_or_else(|_| DEFAULT_TRUSTED_DIRS.iter().map(|s| s.to_string()).collect()),
        None => DEFAULT_TRUSTED_DIRS.iter().map(|s| s.to_string()).collect(),
    };
    Ok(raw.iter().map(|s| expand_tilde(s)).collect())
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
pub async fn revalidate_trust(slug: &str, record: &TrustRecord) -> Result<Provenance> {
    let provenance = match compute_provenance(&record.path) {
        Ok(p) => p,
        Err(err) => {
            let _ = revoke_trust(slug).await;
            return Err(err.context(format!(
                "trusted binary for '{slug}' is no longer readable; trust revoked"
            )));
        }
    };
    if provenance_drifted(&provenance, record) {
        let _ = revoke_trust(slug).await;
        bail!(
            "external agent binary for '{slug}' changed since it was trusted \
             (sha256/device/inode/mtime drift); trust revoked — re-run \
             'libra agent rpc trust {slug}' after verifying the binary"
        );
    }
    // A0-08 defense-in-depth: if the binary's directory was removed from the
    // trusted allowlist since it was trusted, drop it back to quarantine.
    let dirs = read_trusted_dirs().await?;
    if !path_within_trusted_dirs(&provenance.canonical_path, &dirs) {
        let _ = revoke_trust(slug).await;
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
