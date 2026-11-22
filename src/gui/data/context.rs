//! Availability projection for status surfaces (Splash/Welcome/About/
//! Capability map/Search). Derived from the window snapshot; the early probe
//! performs only bounded metadata/database checks.

use sea_orm::ConnectionTrait;

use super::{snapshot::Snapshot, state::SourceState};

/// One data-surface availability row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAvailability {
    pub name: &'static str,
    pub state: SourceState,
    pub label: &'static str,
}

/// Repository and data-surface availability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Availability {
    pub repo: SourceState,
    pub repo_label: String,
    pub branch: String,
    pub head: String,
    pub object_format: String,
    pub sources: Vec<SourceAvailability>,
}

impl Availability {
    /// The placeholder state before the first snapshot arrives.
    pub fn unchecked() -> Self {
        Self {
            repo: SourceState::Unchecked,
            repo_label: "未检查".to_string(),
            branch: "—".to_string(),
            head: "—".to_string(),
            object_format: "—".to_string(),
            sources: SOURCE_NAMES
                .iter()
                .map(|name| SourceAvailability {
                    name,
                    state: SourceState::Unchecked,
                    label: "未检查",
                })
                .collect(),
        }
    }
}

const SOURCE_NAMES: [&str; 5] = ["capture", "operation", "memory", "runs", "history"];

/// A bounded availability probe: metadata plus short database/schema checks
/// (200 ms busy timeout each) so status surfaces get a real verdict quickly.
pub async fn probe_availability(
    scope: Option<&crate::internal::worktree_scope::RequestScope>,
) -> Availability {
    let Some(scope) = scope else {
        // No repository: every surface is absent (never 未检查).
        let mut availability = Availability::unchecked();
        availability.repo = SourceState::Absent;
        availability.repo_label = "no repository".to_string();
        for source in &mut availability.sources {
            source.state = SourceState::Absent;
            source.label = "absent";
        }
        return availability;
    };
    let repo_db = scope.storage.join(crate::utils::util::DATABASE);
    let runs = scope.storage.join("sessions").join("agent-runs");
    let budget = std::time::Duration::from_millis(200);

    let repo_state = probe_repository_database(&repo_db, budget).await;
    let capture_state = probe_capture_database(&scope.storage, budget).await;
    let runs_state = probe_directory(&runs);
    let sources = vec![
        source("capture", capture_state),
        source("operation", repo_state.clone()),
        source("memory", repo_state.clone()),
        source("runs", runs_state),
        source("history", repo_state.clone()),
    ];
    Availability {
        repo: repo_state,
        repo_label: scope
            .worktree_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repository".to_string()),
        branch: "—".to_string(),
        head: "—".to_string(),
        object_format: "—".to_string(),
        sources,
    }
}

/// Probe a repository database with bounded open/schema checks.
async fn probe_repository_database(
    path: &std::path::Path,
    budget: std::time::Duration,
) -> SourceState {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return SourceState::Unreadable {
                kind: std::io::ErrorKind::Other,
            };
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return SourceState::Absent,
        Err(error) => {
            return SourceState::Unreadable { kind: error.kind() };
        }
    }
    if let Err(error) = std::fs::File::open(path) {
        return SourceState::Unreadable { kind: error.kind() };
    }
    let Some(path_str) = path.to_str() else {
        return SourceState::Corrupt;
    };
    let conn = match tokio::time::timeout(
        budget,
        crate::internal::db::open_connection_without_schema_management(path_str, budget),
    )
    .await
    {
        Ok(Ok(conn)) => conn,
        Ok(Err(error)) => return super::state::classify_open_error(&error),
        Err(_) => return SourceState::Busy,
    };
    let state = match crate::internal::db::schema::inspect_schema_for_connection(
        &conn,
        crate::internal::db::schema::DatabaseRole::Repository,
    )
    .await
    {
        Ok(compatibility) => super::state::classify_schema(&compatibility),
        Err(error) => super::state::classify_store_message(&error.to_string()),
    };
    conn.close().await.ok();
    state
}

/// Probe the capture catalog (open_readonly expects the STORAGE directory).
async fn probe_capture_database(
    storage: &std::path::Path,
    budget: std::time::Duration,
) -> SourceState {
    let path = crate::internal::ai::capture::store::capture_database_path(storage);
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return SourceState::Unreadable {
                kind: std::io::ErrorKind::Other,
            };
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return SourceState::Absent,
        Err(error) => {
            return SourceState::Unreadable { kind: error.kind() };
        }
    }
    if let Err(error) = std::fs::File::open(&path) {
        return SourceState::Unreadable { kind: error.kind() };
    }
    // Bound connection establishment too (the busy timeout alone does not).
    let conn = match tokio::time::timeout(
        budget,
        crate::internal::ai::capture::store::open_readonly(storage, budget),
    )
    .await
    {
        Ok(Ok(conn)) => conn,
        Ok(Err(error)) => return super::state::classify_open_error(&error),
        Err(_) => return SourceState::Busy,
    };
    let state = match conn
        .query_one_raw(sea_orm::Statement::from_string(
            conn.get_database_backend(),
            "SELECT MAX(version) AS version FROM schema_versions".to_string(),
        ))
        .await
    {
        Ok(Some(row)) => match row.try_get::<i64>("", "version").ok() {
            Some(version) if version < crate::internal::db::schema::CAPTURE_SCHEMA_VERSION => {
                SourceState::SchemaOld
            }
            Some(version) if version > crate::internal::db::schema::CAPTURE_SCHEMA_VERSION => {
                SourceState::SchemaNew
            }
            Some(_) => SourceState::Available,
            None => SourceState::Corrupt,
        },
        Ok(None) => SourceState::Corrupt,
        Err(error) => super::state::classify_store_message(&error.to_string()),
    };
    conn.close().await.ok();
    state
}

/// Directory existence/type check WITHOUT opening the path (FIFO-safe).
fn probe_directory(path: &std::path::Path) -> SourceState {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => SourceState::Unchecked,
        Ok(_) => SourceState::Unreadable {
            kind: std::io::ErrorKind::NotADirectory,
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => SourceState::Absent,
        Err(error) => SourceState::Unreadable { kind: error.kind() },
    }
}

/// Build availability from a delivered snapshot (or the unchecked placeholder).
pub fn availability(snapshot: Option<&Snapshot>) -> Availability {
    let Some(snapshot) = snapshot else {
        return Availability::unchecked();
    };
    let context = snapshot.context.as_ref();
    let repo = snapshot.state.clone();
    let sources = vec![
        source("capture", snapshot.sessions_state.clone()),
        source("operation", snapshot.operations.state.clone()),
        source("memory", snapshot.memory.state.clone()),
        source("runs", snapshot.runs.state.clone()),
        source("history", snapshot.history_state.clone()),
    ];
    Availability {
        repo,
        repo_label: context
            .map(|context| context.worktree_label.clone())
            .unwrap_or_else(|| "no repository".to_string()),
        branch: context
            .map(|context| context.branch.clone())
            .unwrap_or_else(|| "—".to_string()),
        head: context
            .map(|context| context.head.clone())
            .unwrap_or_else(|| "—".to_string()),
        object_format: context
            .map(|context| context.object_format.clone())
            .unwrap_or_else(|| "—".to_string()),
        sources,
    }
}

fn label_for(state: &SourceState) -> &'static str {
    match state {
        SourceState::Available => "available",
        SourceState::Absent => "absent",
        SourceState::Unchecked => "未检查",
        SourceState::Stale => "stale",
        SourceState::Busy => "busy",
        SourceState::SchemaOld => "schema old",
        SourceState::SchemaNew => "schema new",
        SourceState::Unreadable { .. } => "unreadable",
        SourceState::Corrupt => "corrupt",
        SourceState::Worker => "worker failed",
    }
}

fn source(name: &'static str, state: SourceState) -> SourceAvailability {
    let label = label_for(&state);
    SourceAvailability { name, state, label }
}

/// Availability when the worktree scope could not be resolved at all: every
/// surface stays Unchecked (never absent).
pub fn unresolved_availability() -> Availability {
    let mut availability = Availability::unchecked();
    availability.repo = SourceState::Unchecked;
    availability.repo_label = "unresolved".to_string();
    availability
}

/// Capability rows for a given availability projection (shared by the dialog
/// and the status pages).
pub fn availability_capabilities(
    availability: &Availability,
) -> Vec<super::super::model::Capability> {
    capabilities(availability)
}

/// Merge a terminal repository failure with independently probed sources:
/// the repository error wins, but probe verdicts for unaffected surfaces stay.
pub fn merge_availability(probe: &Availability, snapshot: &Snapshot) -> Availability {
    let mut merged = probe.clone();
    merged.repo = if snapshot.state.is_available() {
        probe.repo.clone()
    } else {
        snapshot.state.clone()
    };
    for source in &mut merged.sources {
        let final_state = match source.name {
            "capture" => snapshot.sessions_state.clone(),
            "operation" => snapshot.operations.state.clone(),
            "memory" => snapshot.memory.state.clone(),
            "runs" => snapshot.runs.state.clone(),
            _ => snapshot.history_state.clone(),
        };
        if final_state != SourceState::Unchecked {
            source.state = final_state.clone();
            source.label = label_for(&final_state);
        }
    }
    merged
}

/// Capability-map rows for the Sessions surface, backed by real states.
pub fn capabilities(availability: &Availability) -> Vec<super::super::model::Capability> {
    availability
        .sources
        .iter()
        .map(|source| super::super::model::Capability {
            capability: source.name.to_string(),
            basis: "window read".to_string(),
            data: source.label.to_string(),
            missing: if source.state.is_available() {
                "—".to_string()
            } else {
                source.label.to_string()
            },
        })
        .collect()
}
