//! W4-06: Code/Agent configuration resolver core.
//!
//! Callers pass an explicit [`RequestScope`] (scope + storage + gitdir pinned
//! from one filesystem observation) and receive bytes + provenance. W4-11
//! migrates security-sensitive loaders onto this API; W4-12 migrates
//! extension/automation loaders. This module does **not** lift the linked
//! worktree preflight (W4-08).

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::{
    internal::{
        config_ownership::{
            CODE_AGENT_CONFIG_OWNERSHIP, ConfigConsumerKind, ConfigOwner, ConfigSurface,
            SurfaceKind,
        },
        worktree_scope::{RequestScope, WorktreeScope},
    },
    utils::util,
};

/// Which layer supplied the primary effective bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigLayer {
    /// Shared repository default under common storage (`.libra` root).
    Repository,
    /// Per-worktree overlay under the local gitdir (linked worktrees only when
    /// distinct from common storage).
    Overlay,
    /// Security surfaces with a readable overlay: repository bytes remain the
    /// base, overlay bytes are exposed for tighten-only composition (W4-11).
    RepositoryWithTighteningOverlay,
}

/// Provenance for a resolved file/directory surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigProvenance {
    pub scope: WorktreeScope,
    pub surface: &'static str,
    pub location: &'static str,
    pub repository_path: PathBuf,
    pub overlay_path: Option<PathBuf>,
    pub winning_layer: ConfigLayer,
    pub consumer: ConfigConsumerKind,
    pub owner: ConfigOwner,
}

/// Resolved configuration plus provenance.
///
/// For **security** surfaces, [`Self::repository_bytes`] is the repository file
/// contents or empty when absent (loaders apply domain defaults, matching
/// sandbox NotFound→default). [`Self::overlay_bytes`] carries any readable
/// overlay so W4-11 loaders can apply domain-specific tighten-only merges.
/// [`Self::bytes`] is the W4-06 effective view (repository base; never an
/// overlay-only replace).
///
/// For **extension** surfaces with optional overlay ownership, [`Self::bytes`]
/// is the winning layer (overlay if present, else repository).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfig {
    pub bytes: Vec<u8>,
    pub repository_bytes: Vec<u8>,
    pub overlay_bytes: Option<Vec<u8>>,
    pub provenance: ConfigProvenance,
}

/// Resolved directory surface paths (W4-12 loaders).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfigDir {
    pub repository_path: PathBuf,
    pub overlay_path: Option<PathBuf>,
    pub provenance: ConfigProvenance,
}

#[derive(Debug, Error)]
pub enum ConfigResolveError {
    #[error(
        "cannot resolve Code/Agent config paths for workdir '{}': {message}",
        workdir.display()
    )]
    Paths { workdir: PathBuf, message: String },
    #[error(
        "unknown Code/Agent config surface '{location}' (not registered in CODE_AGENT_CONFIG_OWNERSHIP)"
    )]
    UnknownSurface { location: String },
    #[error(
        "security config '{location}' repository layer is unreadable at '{}': {message}",
        repository_path.display()
    )]
    SecurityRepositoryUnreadable {
        location: &'static str,
        repository_path: PathBuf,
        message: String,
    },
    #[error(
        "config surface '{location}' is a file/store; use resolve_config_file for files or a directory surface for resolve_config_dir"
    )]
    NotADirectory { location: &'static str },
    #[error(
        "config surface '{location}' is a directory/store; use resolve_config_dir for directories"
    )]
    NotAFile { location: &'static str },
    #[error(
        "RequestScope is internally inconsistent: scope={scope:?}, gitdir-derived={resolved:?} (workdir '{}')",
        workdir.display()
    )]
    ScopeMismatch {
        workdir: PathBuf,
        scope: WorktreeScope,
        resolved: WorktreeScope,
    },
}

impl ConfigResolveError {
    pub fn is_fail_closed_security(&self) -> bool {
        matches!(self, Self::SecurityRepositoryUnreadable { .. })
    }
}

/// Look up a registered surface by `.libra/<location>` name (any kind).
pub fn surface_by_location(location: &str) -> Option<&'static ConfigSurface> {
    CODE_AGENT_CONFIG_OWNERSHIP
        .iter()
        .find(|row| row.location == location)
}

fn surface_file(location: &str) -> Result<&'static ConfigSurface, ConfigResolveError> {
    let surface =
        surface_by_location(location).ok_or_else(|| ConfigResolveError::UnknownSurface {
            location: location.to_string(),
        })?;
    if surface.kind != SurfaceKind::File {
        return Err(ConfigResolveError::NotAFile {
            location: surface.location,
        });
    }
    Ok(surface)
}

fn surface_dir(location: &str) -> Result<&'static ConfigSurface, ConfigResolveError> {
    let surface =
        surface_by_location(location).ok_or_else(|| ConfigResolveError::UnknownSurface {
            location: location.to_string(),
        })?;
    if surface.kind != SurfaceKind::Directory {
        return Err(ConfigResolveError::NotADirectory {
            location: surface.location,
        });
    }
    Ok(surface)
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(a), Ok(b)) => a == b,
        _ => left == right,
    }
}

fn resolve_layer_paths(
    request: &RequestScope,
    location: &str,
    owner: ConfigOwner,
) -> Result<(PathBuf, Option<PathBuf>), ConfigResolveError> {
    // Defend against a forged/stale RequestScope whose scope key disagrees with
    // the pinned gitdir (never re-walk cwd — that is the Main↔Main cross-repo bug).
    let resolved_scope = match util::worktree_id_for_gitdir(&request.gitdir) {
        Some(id) => WorktreeScope::Linked(id),
        None => WorktreeScope::Main,
    };
    if resolved_scope != request.scope {
        return Err(ConfigResolveError::ScopeMismatch {
            workdir: request.workdir.clone(),
            scope: request.scope.clone(),
            resolved: resolved_scope,
        });
    }

    // Storage must be the common dir implied by the pinned gitdir — a caller
    // must not be able to pair repo A's gitdir/overlay with repo B's defaults.
    let storage = util::worktree_common_storage(&request.gitdir).map_err(|error| {
        ConfigResolveError::Paths {
            workdir: request.workdir.clone(),
            message: format!(
                "cannot resolve common storage from pinned gitdir '{}': {error}",
                request.gitdir.display()
            ),
        }
    })?;
    if !paths_equivalent(&storage, &request.storage) {
        return Err(ConfigResolveError::Paths {
            workdir: request.workdir.clone(),
            message: format!(
                "RequestScope.storage '{}' does not match common storage '{}' for gitdir '{}'",
                request.storage.display(),
                storage.display(),
                request.gitdir.display()
            ),
        });
    }

    let repository_path = storage.join(location);
    // Repository-only owners never consult a linked overlay (single common fact).
    let overlay_path = match owner {
        ConfigOwner::Repository | ConfigOwner::RepositoryWithWorkspaceSessionScope => None,
        ConfigOwner::RepositoryWithOptionalOverlay if request.gitdir != storage => {
            Some(request.gitdir.join(location))
        }
        ConfigOwner::RepositoryWithOptionalOverlay => None,
    };
    Ok((repository_path, overlay_path))
}

/// Classification of an optional overlay/repo path without collapsing IO errors
/// into "missing" (unlike [`Path::is_file`] / [`Path::is_dir`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptionalPathKind {
    Missing,
    File,
    Directory,
    Other,
}

fn probe_optional_path_kind(path: &Path) -> Result<OptionalPathKind, io::Error> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(OptionalPathKind::Missing),
        Err(error) => Err(error),
        Ok(meta) if meta.file_type().is_symlink() => {
            // Dangling or unreadable symlink targets must not look like Missing.
            match fs::metadata(path) {
                Ok(target) if target.is_file() => Ok(OptionalPathKind::File),
                Ok(target) if target.is_dir() => Ok(OptionalPathKind::Directory),
                Ok(_) => Ok(OptionalPathKind::Other),
                Err(error) => Err(error),
            }
        }
        Ok(meta) if meta.is_file() => Ok(OptionalPathKind::File),
        Ok(meta) if meta.is_dir() => Ok(OptionalPathKind::Directory),
        Ok(_) => Ok(OptionalPathKind::Other),
    }
}

/// Confirm a directory can be enumerated. Owner-stat succeeds on mode `000`
/// directories, but loaders that `read_dir` would then treat the failure as empty.
/// Consumes every `ReadDir` entry so mid-iteration IO errors (FUSE/NFS) are not
/// deferred past the resolver boundary.
fn require_enumerable_dir(path: &Path) -> Result<(), io::Error> {
    for entry in fs::read_dir(path)? {
        let _ = entry?;
    }
    Ok(())
}

/// Read an optional file: `Ok(None)` only when the path does not exist.
/// Dangling symlinks and other IO errors propagate (fail-closed for security).
fn read_optional_file(path: &Path) -> Result<Option<Vec<u8>>, io::Error> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
        Ok(_) => match fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            // After a successful lstat, NotFound means a dangling symlink (or a
            // TOCTOU delete); never collapse that into an empty default baseline.
            Err(error) => Err(error),
        },
    }
}

fn overlay_wrong_type_error(
    path: &Path,
    location: &str,
    expected: &str,
    actual: OptionalPathKind,
) -> ConfigResolveError {
    ConfigResolveError::Paths {
        workdir: path.to_path_buf(),
        message: format!(
            "config overlay for '{location}' has wrong type (expected {expected}, found {actual:?})"
        ),
    }
}

/// Resolve a registered file surface for an explicit [`RequestScope`].
///
/// Precedence:
/// - [`ConfigOwner::Repository`] (+ workspace-session): repository path only.
/// - Security consumer: repository bytes required only when present; absence is
///   an empty default base (matches current sandbox loader NotFound→default).
///   Unreadable/malformed repository paths fail-closed. Overlay bytes are
///   exposed for tighten-only composition, never as a wholesale replace.
/// - Extension consumer with optional overlay: overlay wins when readable.
pub fn resolve_config_file(
    request: &RequestScope,
    location: &str,
) -> Result<ResolvedConfig, ConfigResolveError> {
    let surface = surface_file(location)?;
    let (repository_path, overlay_path) =
        resolve_layer_paths(request, surface.location, surface.owner)?;

    match surface.consumer {
        ConfigConsumerKind::Security => {
            resolve_security(surface, &request.scope, repository_path, overlay_path)
        }
        ConfigConsumerKind::Extension => {
            resolve_extension(surface, &request.scope, repository_path, overlay_path)
        }
    }
}

/// Resolve a registered directory surface for an explicit [`RequestScope`].
pub fn resolve_config_dir(
    request: &RequestScope,
    location: &str,
) -> Result<ResolvedConfigDir, ConfigResolveError> {
    let surface = surface_dir(location)?;
    let (repository_path, overlay_path) =
        resolve_layer_paths(request, surface.location, surface.owner)?;

    let (overlay_path, overlay_is_dir) = match overlay_path {
        None => (None, false),
        Some(path) => {
            let kind =
                probe_optional_path_kind(&path).map_err(|error| ConfigResolveError::Paths {
                    workdir: path.clone(),
                    message: format!(
                        "config directory overlay for '{}' is inaccessible: {error}",
                        surface.location
                    ),
                })?;
            match kind {
                OptionalPathKind::Missing => (Some(path), false),
                OptionalPathKind::Directory => {
                    require_enumerable_dir(&path).map_err(|error| ConfigResolveError::Paths {
                        workdir: path.clone(),
                        message: format!(
                            "config directory overlay for '{}' is inaccessible: {error}",
                            surface.location
                        ),
                    })?;
                    (Some(path), true)
                }
                OptionalPathKind::File | OptionalPathKind::Other => {
                    return Err(overlay_wrong_type_error(
                        &path,
                        surface.location,
                        "directory",
                        kind,
                    ));
                }
            }
        }
    };

    let winning_layer = match surface.consumer {
        ConfigConsumerKind::Security => {
            let repo_kind = probe_optional_path_kind(&repository_path).map_err(|error| {
                ConfigResolveError::SecurityRepositoryUnreadable {
                    location: surface.location,
                    repository_path: repository_path.clone(),
                    message: error.to_string(),
                }
            })?;
            match repo_kind {
                OptionalPathKind::Missing => {}
                OptionalPathKind::Directory => {
                    require_enumerable_dir(&repository_path).map_err(|error| {
                        ConfigResolveError::SecurityRepositoryUnreadable {
                            location: surface.location,
                            repository_path: repository_path.clone(),
                            message: error.to_string(),
                        }
                    })?;
                }
                OptionalPathKind::File | OptionalPathKind::Other => {
                    return Err(ConfigResolveError::SecurityRepositoryUnreadable {
                        location: surface.location,
                        repository_path: repository_path.clone(),
                        message: format!("expected directory, found {repo_kind:?}"),
                    });
                }
            }
            if overlay_is_dir {
                ConfigLayer::RepositoryWithTighteningOverlay
            } else {
                ConfigLayer::Repository
            }
        }
        ConfigConsumerKind::Extension => {
            // Overlay-wins: do not require an accessible repository base when the
            // overlay directory is already selected.
            if overlay_is_dir {
                ConfigLayer::Overlay
            } else {
                // Without an overlay, still surface EACCES / wrong-type on the
                // repository path (must not look like a clean miss).
                match probe_optional_path_kind(&repository_path).map_err(|error| {
                    ConfigResolveError::Paths {
                        workdir: repository_path.clone(),
                        message: format!(
                            "extension config directory '{}' is inaccessible: {error}",
                            surface.location
                        ),
                    }
                })? {
                    OptionalPathKind::Missing => ConfigLayer::Repository,
                    OptionalPathKind::Directory => {
                        require_enumerable_dir(&repository_path).map_err(|error| {
                            ConfigResolveError::Paths {
                                workdir: repository_path.clone(),
                                message: format!(
                                    "extension config directory '{}' is inaccessible: {error}",
                                    surface.location
                                ),
                            }
                        })?;
                        ConfigLayer::Repository
                    }
                    kind @ (OptionalPathKind::File | OptionalPathKind::Other) => {
                        return Err(ConfigResolveError::Paths {
                            workdir: repository_path.clone(),
                            message: format!(
                                "extension config directory '{}' has wrong type (expected directory, found {kind:?})",
                                surface.location
                            ),
                        });
                    }
                }
            }
        }
    };

    Ok(ResolvedConfigDir {
        repository_path: repository_path.clone(),
        overlay_path: overlay_path.clone(),
        provenance: ConfigProvenance {
            scope: request.scope.clone(),
            surface: surface.surface,
            location: surface.location,
            repository_path,
            overlay_path,
            winning_layer,
            consumer: surface.consumer,
            owner: surface.owner,
        },
    })
}

fn resolve_security(
    surface: &ConfigSurface,
    scope: &WorktreeScope,
    repository_path: PathBuf,
    overlay_path: Option<PathBuf>,
) -> Result<ResolvedConfig, ConfigResolveError> {
    let repository_bytes = read_optional_file(&repository_path)
        .map_err(|error| ConfigResolveError::SecurityRepositoryUnreadable {
            location: surface.location,
            repository_path: repository_path.clone(),
            message: error.to_string(),
        })?
        .unwrap_or_default();

    let overlay_bytes = match overlay_path.as_ref() {
        Some(path) => {
            match probe_optional_path_kind(path).map_err(|error| ConfigResolveError::Paths {
                workdir: path.clone(),
                message: format!(
                    "security overlay for '{}' is inaccessible: {error}",
                    surface.location
                ),
            })? {
                OptionalPathKind::Missing => None,
                OptionalPathKind::File => {
                    Some(fs::read(path).map_err(|error| ConfigResolveError::Paths {
                        workdir: path.clone(),
                        message: format!(
                            "security overlay for '{}' is unreadable: {error}",
                            surface.location
                        ),
                    })?)
                }
                kind @ (OptionalPathKind::Directory | OptionalPathKind::Other) => {
                    return Err(overlay_wrong_type_error(
                        path,
                        surface.location,
                        "file",
                        kind,
                    ));
                }
            }
        }
        None => None,
    };

    let winning_layer = if overlay_bytes.is_some() {
        ConfigLayer::RepositoryWithTighteningOverlay
    } else {
        ConfigLayer::Repository
    };

    // Effective view stays repository-based (never overlay-only replace). Domain
    // loaders (W4-11) must intersect `overlay_bytes` for tighten-only adds.
    Ok(ResolvedConfig {
        bytes: repository_bytes.clone(),
        repository_bytes,
        overlay_bytes,
        provenance: ConfigProvenance {
            scope: scope.clone(),
            surface: surface.surface,
            location: surface.location,
            repository_path,
            overlay_path,
            winning_layer,
            consumer: ConfigConsumerKind::Security,
            owner: surface.owner,
        },
    })
}

fn resolve_extension(
    surface: &ConfigSurface,
    scope: &WorktreeScope,
    repository_path: PathBuf,
    overlay_path: Option<PathBuf>,
) -> Result<ResolvedConfig, ConfigResolveError> {
    if let Some(overlay) = overlay_path.as_ref() {
        // Probe first so EACCES / wrong-type are never treated as "no overlay".
        match probe_optional_path_kind(overlay).map_err(|error| ConfigResolveError::Paths {
            workdir: overlay.clone(),
            message: format!(
                "extension overlay for '{}' is inaccessible: {error}",
                surface.location
            ),
        })? {
            OptionalPathKind::Missing => {}
            OptionalPathKind::File => {
                let bytes = fs::read(overlay).map_err(|error| ConfigResolveError::Paths {
                    workdir: overlay.clone(),
                    message: format!(
                        "extension overlay for '{}' is unreadable: {error}",
                        surface.location
                    ),
                })?;
                // Overlay wins: an inaccessible/missing repository base must not
                // block the override (repository_bytes stay empty when unread).
                let repository_bytes = match read_optional_file(&repository_path) {
                    Ok(Some(bytes)) => bytes,
                    Ok(None) => Vec::new(),
                    Err(_) => Vec::new(),
                };
                return Ok(ResolvedConfig {
                    bytes: bytes.clone(),
                    repository_bytes,
                    overlay_bytes: Some(bytes),
                    provenance: ConfigProvenance {
                        scope: scope.clone(),
                        surface: surface.surface,
                        location: surface.location,
                        repository_path,
                        overlay_path,
                        winning_layer: ConfigLayer::Overlay,
                        consumer: ConfigConsumerKind::Extension,
                        owner: surface.owner,
                    },
                });
            }
            kind @ (OptionalPathKind::Directory | OptionalPathKind::Other) => {
                return Err(overlay_wrong_type_error(
                    overlay,
                    surface.location,
                    "file",
                    kind,
                ));
            }
        }
    }

    match read_optional_file(&repository_path).map_err(|error| ConfigResolveError::Paths {
        workdir: repository_path.clone(),
        message: error.to_string(),
    })? {
        Some(bytes) => Ok(ResolvedConfig {
            bytes: bytes.clone(),
            repository_bytes: bytes,
            overlay_bytes: None,
            provenance: ConfigProvenance {
                scope: scope.clone(),
                surface: surface.surface,
                location: surface.location,
                repository_path,
                overlay_path,
                winning_layer: ConfigLayer::Repository,
                consumer: ConfigConsumerKind::Extension,
                owner: surface.owner,
            },
        }),
        // Absence is an empty default baseline (AgentsConfig::load_or_default).
        None => Ok(ResolvedConfig {
            bytes: Vec::new(),
            repository_bytes: Vec::new(),
            overlay_bytes: None,
            provenance: ConfigProvenance {
                scope: scope.clone(),
                surface: surface.surface,
                location: surface.location,
                repository_path,
                overlay_path,
                winning_layer: ConfigLayer::Repository,
                consumer: ConfigConsumerKind::Extension,
                owner: surface.owner,
            },
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::config_ownership::ConfigConsumerKind;

    #[test]
    fn inventory_marks_sandbox_hooks_and_config_as_security() {
        let sandbox = surface_by_location("sandbox.toml").expect("sandbox.toml registered");
        assert_eq!(sandbox.consumer, ConfigConsumerKind::Security);
        let hooks = surface_by_location("hooks.json").expect("hooks.json registered");
        assert_eq!(hooks.consumer, ConfigConsumerKind::Security);
        let config = surface_by_location("config.toml").expect("config.toml registered");
        assert_eq!(
            config.consumer,
            ConfigConsumerKind::Security,
            "config.toml carries [approval]; treat as security until section merge"
        );
    }

    #[test]
    fn inventory_marks_automations_as_extension() {
        let automations =
            surface_by_location("automations.toml").expect("automations.toml registered");
        assert_eq!(automations.consumer, ConfigConsumerKind::Extension);
    }
}
