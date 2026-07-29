//! ScorpioFS remote-worktree backend protocol.
//!
//! This module is intentionally independent from `command::*`. It provides the
//! transport and persistent data types used to attach a Libra linked worktree
//! to an externally managed ScorpioFS/Antares mount.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::time::{Instant, sleep};
use url::Url;

use crate::internal::worktree_backend::{
    BackendDescriptor, BackendHealth, BackendKind, BackendMountRequest, BackendMountSession,
    BackendMountSource, WorktreeBackendDriver, WorktreeBackendError,
};
pub use crate::internal::worktree_backend::{BackendLifecycle, ChangeKind, ChangeSet, ChangedPath};

pub const BACKEND_SCHEMA_VERSION: u32 = 1;
pub const PROTOCOL_VERSION: u32 = 1;

pub const CAPABILITY_MOUNT_V1: &str = "mount.v1";
pub const CAPABILITY_READY_V1: &str = "ready.v1";
pub const CAPABILITY_CHANGES_V1: &str = "changes.v1";
pub const CAPABILITY_BASE_SNAPSHOT_V1: &str = "base-snapshot.v1";
pub const BACKEND_RECORD_FILE: &str = "backend.json";
pub const DESIRED_STATE_FILE: &str = "scorpiofs/state.json";
pub const DESIRED_STATE_LOCK_FILE: &str = "scorpiofs/state.lock";

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_READY_POLL_INTERVAL: Duration = Duration::from_millis(250);

fn default_service_name() -> String {
    "scorpiofs".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BackendTransport {
    ManagedCrate,
    #[default]
    ExternalHttp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScorpioFsBackendRecord {
    pub schema_version: u32,
    pub backend: String,
    #[serde(default)]
    pub transport: BackendTransport,
    pub endpoint: String,
    pub mount_id: String,
    pub job_id: String,
    pub remote_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cl: Option<String>,
}

impl ScorpioFsBackendRecord {
    pub fn new(
        endpoint: &Url,
        mount: &MountResponse,
        request: &MountRequest,
    ) -> Result<Self, BackendError> {
        Self::new_with_transport(endpoint, mount, request, BackendTransport::ExternalHttp)
    }

    pub fn new_with_transport(
        endpoint: &Url,
        mount: &MountResponse,
        request: &MountRequest,
        transport: BackendTransport,
    ) -> Result<Self, BackendError> {
        validate_identifier("mount_id", &mount.mount_id)?;
        validate_identifier("job_id", &request.job_id)?;
        validate_remote_path(&request.path)?;

        Ok(Self {
            schema_version: BACKEND_SCHEMA_VERSION,
            backend: "scorpiofs".to_string(),
            transport,
            endpoint: endpoint.as_str().trim_end_matches('/').to_string(),
            mount_id: mount.mount_id.clone(),
            job_id: request.job_id.clone(),
            remote_path: request.path.clone(),
            base_oid: mount.base_oid.clone().or_else(|| request.base_oid.clone()),
            cl: request.cl.clone(),
        })
    }

    pub fn validate(&self) -> Result<(), BackendError> {
        if self.schema_version != BACKEND_SCHEMA_VERSION {
            return Err(BackendError::UnsupportedRecordVersion {
                found: self.schema_version,
                supported: BACKEND_SCHEMA_VERSION,
            });
        }
        if self.backend != "scorpiofs" {
            return Err(BackendError::InvalidRecord(format!(
                "expected backend 'scorpiofs', found '{}'",
                self.backend
            )));
        }

        validate_endpoint(&self.endpoint)?;
        validate_identifier("mount_id", &self.mount_id)?;
        validate_identifier("job_id", &self.job_id)?;
        validate_remote_path(&self.remote_path)
    }

    pub fn load(gitdir: &Path) -> Result<Self, BackendError> {
        let path = gitdir.join(BACKEND_RECORD_FILE);
        let data = fs::read(&path).map_err(|source| BackendError::RecordIo {
            path: path.clone(),
            source,
        })?;
        let record =
            serde_json::from_slice::<Self>(&data).map_err(|source| BackendError::RecordDecode {
                path: path.clone(),
                source,
            })?;
        record.validate()?;
        Ok(record)
    }

    pub fn save(&self, gitdir: &Path) -> Result<(), BackendError> {
        self.validate()?;
        fs::create_dir_all(gitdir).map_err(|source| BackendError::RecordIo {
            path: gitdir.to_path_buf(),
            source,
        })?;

        let path = gitdir.join(BACKEND_RECORD_FILE);
        let temporary = gitdir.join(format!(".{BACKEND_RECORD_FILE}.tmp-{}", std::process::id()));
        let data = serde_json::to_vec_pretty(self).map_err(BackendError::RecordEncode)?;
        fs::write(&temporary, data).map_err(|source| BackendError::RecordIo {
            path: temporary.clone(),
            source,
        })?;
        if let Err(source) = replace_file(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(BackendError::RecordIo { path, source });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedWorkerRecord {
    pub pid: u32,
    pub endpoint: String,
    pub config_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesiredMountRecord {
    pub request: MountRequest,
    pub transport: BackendTransport,
    pub lifecycle: BackendLifecycle,
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mountpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraScorpioFsState {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<ManagedWorkerRecord>,
    #[serde(default)]
    pub mounts: BTreeMap<String, DesiredMountRecord>,
}

pub struct ScorpioFsStateLock {
    file: fs::File,
}

impl ScorpioFsStateLock {
    pub fn acquire(storage: &Path) -> Result<Self, BackendError> {
        let path = storage.join(DESIRED_STATE_LOCK_FILE);
        let parent = path.parent().ok_or_else(|| {
            BackendError::InvalidRecord("ScorpioFS lock path has no parent".to_string())
        })?;
        fs::create_dir_all(parent).map_err(|source| BackendError::StateIo {
            path: parent.to_path_buf(),
            source,
        })?;
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|source| BackendError::StateIo {
                path: path.clone(),
                source,
            })?;
        fs2::FileExt::lock_exclusive(&file)
            .map_err(|source| BackendError::StateIo { path, source })?;
        Ok(Self { file })
    }
}

impl Drop for ScorpioFsStateLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

impl Default for LibraScorpioFsState {
    fn default() -> Self {
        Self {
            schema_version: 1,
            worker: None,
            mounts: BTreeMap::new(),
        }
    }
}

impl LibraScorpioFsState {
    pub fn load(storage: &Path) -> Result<Self, BackendError> {
        let path = storage.join(DESIRED_STATE_FILE);
        let data = match fs::read(&path) {
            Ok(data) => data,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => return Err(BackendError::StateIo { path, source }),
        };
        let state =
            serde_json::from_slice::<Self>(&data).map_err(|source| BackendError::StateDecode {
                path: path.clone(),
                source,
            })?;
        if state.schema_version != 1 {
            return Err(BackendError::InvalidRecord(format!(
                "unsupported Libra ScorpioFS state version {}",
                state.schema_version
            )));
        }
        Ok(state)
    }

    pub fn save(&self, storage: &Path) -> Result<(), BackendError> {
        let path = storage.join(DESIRED_STATE_FILE);
        let parent = path.parent().ok_or_else(|| {
            BackendError::InvalidRecord("ScorpioFS state path has no parent".to_string())
        })?;
        fs::create_dir_all(parent).map_err(|source| BackendError::StateIo {
            path: parent.to_path_buf(),
            source,
        })?;
        let temporary = parent.join(format!(".state.json.tmp-{}", std::process::id()));
        let data = serde_json::to_vec_pretty(self).map_err(BackendError::RecordEncode)?;
        fs::write(&temporary, data).map_err(|source| BackendError::StateIo {
            path: temporary.clone(),
            source,
        })?;
        if let Err(source) = replace_file(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(BackendError::StateIo { path, source });
        }
        Ok(())
    }

    pub fn begin_mount(
        &mut self,
        request: MountRequest,
        transport: BackendTransport,
        endpoint: String,
    ) -> Result<(), BackendError> {
        request.validate()?;
        if let Some(existing) = self.mounts.get(&request.job_id) {
            if existing.request.path != request.path || existing.request.cl != request.cl {
                return Err(BackendError::InvalidRecord(format!(
                    "ScorpioFS job '{}' is already assigned to '{}' with CL {:?}",
                    request.job_id, existing.request.path, existing.request.cl
                )));
            }
        }
        self.mounts.insert(
            request.job_id.clone(),
            DesiredMountRecord {
                request,
                transport,
                lifecycle: BackendLifecycle::Mounting,
                endpoint,
                mount_id: None,
                mountpoint: None,
                worktree_id: None,
                last_error: None,
            },
        );
        Ok(())
    }

    pub fn mark_ready(
        &mut self,
        job_id: &str,
        mount: &MountResponse,
        worktree_id: &str,
    ) -> Result<(), BackendError> {
        let desired = self.mounts.get_mut(job_id).ok_or_else(|| {
            BackendError::InvalidRecord(format!("missing desired mount for job '{job_id}'"))
        })?;
        desired.lifecycle = BackendLifecycle::Ready;
        desired.mount_id = Some(mount.mount_id.clone());
        desired.mountpoint = Some(mount.mountpoint.clone());
        desired.worktree_id = Some(worktree_id.to_string());
        desired.last_error = None;
        Ok(())
    }

    pub fn mark_error(&mut self, job_id: &str, error: impl Into<String>) {
        if let Some(desired) = self.mounts.get_mut(job_id) {
            desired.lifecycle = BackendLifecycle::RecoverableError;
            desired.last_error = Some(error.into());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub protocol_version: u32,
    pub service: String,
    #[serde(default)]
    pub service_version: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl ServiceInfo {
    pub fn validate(&self) -> Result<(), BackendError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(BackendError::UnsupportedProtocolVersion {
                found: self.protocol_version,
                supported: PROTOCOL_VERSION,
            });
        }
        if self.service != "scorpiofs" {
            return Err(BackendError::UnexpectedService(self.service.clone()));
        }
        Ok(())
    }

    pub fn supports(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|item| item == capability)
    }

    pub fn require(&self, capability: &'static str) -> Result<(), BackendError> {
        if self.supports(capability) {
            Ok(())
        } else {
            Err(BackendError::MissingCapability(capability))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountRequest {
    pub job_id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cl: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_oid: Option<String>,
}

impl MountRequest {
    pub fn validate(&self) -> Result<(), BackendError> {
        validate_identifier("job_id", &self.job_id)?;
        validate_remote_path(&self.path)?;
        if let Some(base_oid) = self.base_oid.as_deref() {
            validate_object_id(base_oid)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountResponse {
    #[serde(alias = "id")]
    pub mount_id: String,
    pub mountpoint: String,
    #[serde(default)]
    pub base_oid: Option<String>,
    #[serde(default)]
    pub ready: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadyResponse {
    #[serde(default)]
    pub mount_id: Option<String>,
    pub ready: bool,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("invalid ScorpioFS endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("invalid ScorpioFS backend record: {0}")]
    InvalidRecord(String),
    #[error("failed to discover ScorpioFS worktree metadata: {0}")]
    MetadataDiscovery(#[source] io::Error),
    #[error(
        "unsupported ScorpioFS backend record version {found}; this Libra supports version {supported}"
    )]
    UnsupportedRecordVersion { found: u32, supported: u32 },
    #[error(
        "unsupported ScorpioFS protocol version {found}; this Libra supports version {supported}"
    )]
    UnsupportedProtocolVersion { found: u32, supported: u32 },
    #[error("unexpected ScorpioFS control service '{0}'")]
    UnexpectedService(String),
    #[error("failed to read or write ScorpioFS backend record '{}': {source}", path.display())]
    RecordIo {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to encode ScorpioFS backend record: {0}")]
    RecordEncode(serde_json::Error),
    #[error("failed to decode ScorpioFS backend record '{}': {source}", path.display())]
    RecordDecode {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to read or write Libra ScorpioFS state '{}': {source}", path.display())]
    StateIo {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to decode Libra ScorpioFS state '{}': {source}", path.display())]
    StateDecode {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid {field}: {message}")]
    InvalidIdentifier {
        field: &'static str,
        message: String,
    },
    #[error("invalid ScorpioFS remote path: {0}")]
    InvalidRemotePath(String),
    #[error("invalid ScorpioFS changed path: {0}")]
    InvalidChangedPath(String),
    #[error("invalid base object id: {0}")]
    InvalidObjectId(String),
    #[error("ScorpioFS does not advertise required capability '{0}'")]
    MissingCapability(&'static str),
    #[error("ScorpioFS request '{operation}' failed: {source}")]
    Request {
        operation: &'static str,
        #[source]
        source: anyhow::Error,
    },
    #[error("ScorpioFS request '{operation}' returned HTTP {status}: {message}")]
    HttpStatus {
        operation: &'static str,
        status: StatusCode,
        message: String,
    },
    #[error("ScorpioFS mount '{mount_id}' did not become ready within {timeout:?}")]
    ReadinessTimeout { mount_id: String, timeout: Duration },
}

fn replace_file(temporary: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(windows)]
    if destination.exists() {
        fs::remove_file(destination)?;
    }
    fs::rename(temporary, destination)
}

#[async_trait]
pub trait ScorpioFsControl: Send + Sync {
    async fn service_info(&self) -> Result<ServiceInfo, BackendError>;
    async fn mount(&self, request: &MountRequest) -> Result<MountResponse, BackendError>;
    async fn ready(&self, mount_id: &str) -> Result<ReadyResponse, BackendError>;
    async fn changes(&self, mount_id: &str) -> Result<ChangeSet, BackendError>;
    async fn delete_by_job(&self, job_id: &str) -> Result<(), BackendError>;
}

#[derive(Debug, Clone)]
pub struct HttpScorpioFsClient {
    endpoint: Url,
    client: Client,
}

impl HttpScorpioFsClient {
    pub fn new(endpoint: &str) -> Result<Self, BackendError> {
        let endpoint = validate_endpoint(endpoint)?;
        let client = Client::builder()
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .build()
            .map_err(|source| BackendError::Request {
                operation: "create client",
                source: source.into(),
            })?;
        Ok(Self { endpoint, client })
    }

    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    pub async fn wait_until_ready(
        &self,
        mount_id: &str,
        timeout: Duration,
    ) -> Result<ReadyResponse, BackendError> {
        validate_identifier("mount_id", mount_id)?;
        let deadline = Instant::now() + timeout;

        loop {
            let response = self.ready(mount_id).await?;
            if response.ready {
                return Ok(response);
            }
            if Instant::now() >= deadline {
                return Err(BackendError::ReadinessTimeout {
                    mount_id: mount_id.to_string(),
                    timeout,
                });
            }
            sleep(DEFAULT_READY_POLL_INTERVAL).await;
        }
    }

    fn url(&self, segments: &[&str]) -> Result<Url, BackendError> {
        let mut url = self.endpoint.clone();
        {
            let mut path = url.path_segments_mut().map_err(|_| {
                BackendError::InvalidEndpoint(
                    "the endpoint cannot be used as a hierarchical URL".to_string(),
                )
            })?;
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
        }
        Ok(url)
    }

    async fn status_error(operation: &'static str, response: reqwest::Response) -> BackendError {
        let status = response.status();
        let message = response
            .text()
            .await
            .unwrap_or_else(|_| "response body was unreadable".to_string());
        BackendError::HttpStatus {
            operation,
            status,
            message: truncate_message(&message),
        }
    }
}

#[async_trait]
impl ScorpioFsControl for HttpScorpioFsClient {
    async fn service_info(&self) -> Result<ServiceInfo, BackendError> {
        let operation = "service info";
        let response = self
            .client
            .get(self.url(&["health"])?)
            .send()
            .await
            .with_context(|| "failed to reach the ScorpioFS health endpoint")
            .map_err(|source| BackendError::Request { operation, source })?;
        if !response.status().is_success() {
            return Err(Self::status_error(operation, response).await);
        }

        #[derive(Deserialize)]
        struct Health {
            #[serde(default = "default_service_name")]
            service: String,
            #[serde(default, alias = "version")]
            service_version: Option<String>,
            #[serde(default)]
            protocol_version: Option<u32>,
            #[serde(default)]
            capabilities: Vec<String>,
        }

        let health: Health = response
            .json()
            .await
            .with_context(|| "ScorpioFS health returned invalid JSON")
            .map_err(|source| BackendError::Request { operation, source })?;
        let info = ServiceInfo {
            protocol_version: health.protocol_version.unwrap_or(PROTOCOL_VERSION),
            service: health.service,
            service_version: health.service_version,
            capabilities: health.capabilities,
        };
        info.validate()?;
        Ok(info)
    }

    async fn mount(&self, request: &MountRequest) -> Result<MountResponse, BackendError> {
        request.validate()?;
        let operation = "mount";
        let response = self
            .client
            .post(self.url(&["mounts"])?)
            .json(request)
            .send()
            .await
            .with_context(|| "failed to send the ScorpioFS mount request")
            .map_err(|source| BackendError::Request { operation, source })?;
        if !response.status().is_success() {
            return Err(Self::status_error(operation, response).await);
        }
        let mount: MountResponse = response
            .json()
            .await
            .with_context(|| "ScorpioFS mount returned invalid JSON")
            .map_err(|source| BackendError::Request { operation, source })?;
        validate_identifier("mount_id", &mount.mount_id)?;
        if mount.mountpoint.trim().is_empty() {
            return Err(BackendError::InvalidRecord(
                "ScorpioFS returned an empty mountpoint".to_string(),
            ));
        }
        Ok(mount)
    }

    async fn ready(&self, mount_id: &str) -> Result<ReadyResponse, BackendError> {
        validate_identifier("mount_id", mount_id)?;
        let operation = "mount readiness";
        let response = self
            .client
            .get(self.url(&["mounts", mount_id, "ready"])?)
            .send()
            .await
            .with_context(|| format!("failed to query ScorpioFS mount '{mount_id}' readiness"))
            .map_err(|source| BackendError::Request { operation, source })?;
        if !response.status().is_success() {
            return Err(Self::status_error(operation, response).await);
        }
        response
            .json()
            .await
            .with_context(|| "ScorpioFS readiness returned invalid JSON")
            .map_err(|source| BackendError::Request { operation, source })
    }

    async fn changes(&self, mount_id: &str) -> Result<ChangeSet, BackendError> {
        validate_identifier("mount_id", mount_id)?;
        let operation = "changed paths";
        let response = self
            .client
            .get(self.url(&["mounts", mount_id, "changes"])?)
            .send()
            .await
            .with_context(|| format!("failed to query ScorpioFS mount '{mount_id}' changes"))
            .map_err(|source| BackendError::Request { operation, source })?;
        if !response.status().is_success() {
            return Err(Self::status_error(operation, response).await);
        }
        let changes: ChangeSet = response
            .json()
            .await
            .with_context(|| "ScorpioFS changed paths returned invalid JSON")
            .map_err(|source| BackendError::Request { operation, source })?;
        validate_identifier("mount_id", &changes.mount_id)?;
        changes
            .validate()
            .map_err(|error| BackendError::InvalidChangedPath(error.to_string()))?;
        Ok(changes)
    }

    async fn delete_by_job(&self, job_id: &str) -> Result<(), BackendError> {
        validate_identifier("job_id", job_id)?;
        let operation = "delete mount";
        let response = self
            .client
            .delete(self.url(&["mounts", "by-job", job_id])?)
            .send()
            .await
            .with_context(|| format!("failed to delete ScorpioFS job '{job_id}'"))
            .map_err(|source| BackendError::Request { operation, source })?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(());
        }
        if !response.status().is_success() {
            return Err(Self::status_error(operation, response).await);
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ScorpioFsDriver {
    client: HttpScorpioFsClient,
}

impl ScorpioFsDriver {
    pub fn new(client: HttpScorpioFsClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl WorktreeBackendDriver for ScorpioFsDriver {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor::scorpiofs(true)
    }

    async fn mount(
        &self,
        request: &BackendMountRequest,
    ) -> Result<BackendMountSession, WorktreeBackendError> {
        let (remote_path, base_oid, change_layer) = match &request.source {
            BackendMountSource::RemoteProjection {
                remote_path,
                base_oid,
                change_layer,
            } => (remote_path, base_oid, change_layer),
            BackendMountSource::LocalDirectory { .. } => {
                return Err(WorktreeBackendError::UnsupportedSource {
                    backend: BackendKind::ScorpioFs,
                    detail: "local_directory",
                });
            }
            BackendMountSource::PersistentVolume { .. } => {
                return Err(WorktreeBackendError::UnsupportedSource {
                    backend: BackendKind::ScorpioFs,
                    detail: "persistent_volume",
                });
            }
        };

        let service = self.client.service_info().await.map_err(|error| {
            WorktreeBackendError::operation(BackendKind::ScorpioFs, "service_info", error)
        })?;
        for capability in [
            CAPABILITY_MOUNT_V1,
            CAPABILITY_READY_V1,
            CAPABILITY_CHANGES_V1,
        ] {
            service.require(capability).map_err(|error| {
                WorktreeBackendError::operation(
                    BackendKind::ScorpioFs,
                    "capability_negotiation",
                    error,
                )
            })?;
        }

        let scorpio_request = MountRequest {
            job_id: request.instance_id.clone(),
            path: remote_path.clone(),
            cl: change_layer.clone(),
            base_oid: base_oid.clone(),
        };
        let mount = self.client.mount(&scorpio_request).await.map_err(|error| {
            WorktreeBackendError::operation(BackendKind::ScorpioFs, "mount", error)
        })?;
        if mount.ready != Some(true) {
            if let Err(error) = self
                .client
                .wait_until_ready(
                    &mount.mount_id,
                    Duration::from_secs(request.ready_timeout_secs),
                )
                .await
            {
                let _ = self.client.delete_by_job(&request.instance_id).await;
                return Err(WorktreeBackendError::operation(
                    BackendKind::ScorpioFs,
                    "wait_until_ready",
                    error,
                ));
            }
        }

        Ok(BackendMountSession {
            backend: BackendKind::ScorpioFs,
            session_id: mount.mount_id,
            mountpoint: PathBuf::from(mount.mountpoint),
            cleanup_key: request.instance_id.clone(),
            base_oid: mount.base_oid.or_else(|| base_oid.clone()),
        })
    }

    async fn health(
        &self,
        session: &BackendMountSession,
    ) -> Result<BackendHealth, WorktreeBackendError> {
        let response = self
            .client
            .ready(&session.session_id)
            .await
            .map_err(|error| {
                WorktreeBackendError::operation(BackendKind::ScorpioFs, "health", error)
            })?;
        Ok(BackendHealth {
            ready: response.ready,
            detail: response.detail.or(response.status),
        })
    }

    async fn changed_paths(
        &self,
        session: &BackendMountSession,
    ) -> Result<Option<ChangeSet>, WorktreeBackendError> {
        self.client
            .changes(&session.session_id)
            .await
            .map(Some)
            .map_err(|error| {
                WorktreeBackendError::operation(BackendKind::ScorpioFs, "changed_paths", error)
            })
    }

    async fn unmount(&self, session: &BackendMountSession) -> Result<(), WorktreeBackendError> {
        self.client
            .delete_by_job(&session.cleanup_key)
            .await
            .map_err(|error| {
                WorktreeBackendError::operation(BackendKind::ScorpioFs, "unmount", error)
            })
    }
}

/// Return the changed-path set for the current worktree when it is backed by
/// ScorpioFS. Ordinary Libra worktrees return `None` without making a request.
pub async fn current_worktree_changes() -> Result<Option<ChangeSet>, BackendError> {
    let gitdir = crate::utils::util::try_get_worktree_gitdir(None)
        .map_err(BackendError::MetadataDiscovery)?;
    let record_path = gitdir.join(BACKEND_RECORD_FILE);
    if !record_path.exists() {
        return Ok(None);
    }

    let record = ScorpioFsBackendRecord::load(&gitdir)?;
    let client = HttpScorpioFsClient::new(&record.endpoint)?;
    let driver = ScorpioFsDriver::new(client);
    let session = BackendMountSession {
        backend: BackendKind::ScorpioFs,
        session_id: record.mount_id,
        mountpoint: PathBuf::new(),
        cleanup_key: record.job_id,
        base_oid: record.base_oid,
    };
    driver
        .changed_paths(&session)
        .await
        .map_err(|error| BackendError::InvalidRecord(error.to_string()))
}

fn validate_endpoint(endpoint: &str) -> Result<Url, BackendError> {
    let mut url =
        Url::parse(endpoint).map_err(|error| BackendError::InvalidEndpoint(error.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(BackendError::InvalidEndpoint(
            "only http and https are supported by the initial transport".to_string(),
        ));
    }
    if url.host_str().is_none() {
        return Err(BackendError::InvalidEndpoint(
            "the endpoint must include a host".to_string(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(BackendError::InvalidEndpoint(
            "credentials must not be embedded in the endpoint URL".to_string(),
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(BackendError::InvalidEndpoint(
            "query strings and fragments are not allowed".to_string(),
        ));
    }

    let normalized = url.path().trim_end_matches('/').to_string();
    url.set_path(&normalized);
    Ok(url)
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), BackendError> {
    if value.is_empty() {
        return Err(BackendError::InvalidIdentifier {
            field,
            message: "value cannot be empty".to_string(),
        });
    }
    if value.len() > 255 {
        return Err(BackendError::InvalidIdentifier {
            field,
            message: "value exceeds 255 bytes".to_string(),
        });
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(BackendError::InvalidIdentifier {
            field,
            message: "only ASCII letters, digits, '-', '_', and '.' are allowed".to_string(),
        });
    }
    Ok(())
}

fn validate_remote_path(path: &str) -> Result<(), BackendError> {
    if !path.starts_with('/') {
        return Err(BackendError::InvalidRemotePath(
            "path must be absolute within the monorepo".to_string(),
        ));
    }
    if path.contains('\0') || path.split('/').any(|part| part == "..") {
        return Err(BackendError::InvalidRemotePath(
            "path must not contain NUL or parent traversal".to_string(),
        ));
    }
    Ok(())
}

fn validate_object_id(object_id: &str) -> Result<(), BackendError> {
    if !matches!(object_id.len(), 40 | 64)
        || !object_id.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(BackendError::InvalidObjectId(
            "expected a 40- or 64-character hexadecimal object id".to_string(),
        ));
    }
    Ok(())
}

fn truncate_message(message: &str) -> String {
    const MAX_CHARS: usize = 2048;
    let message = message.trim();
    if message.chars().count() <= MAX_CHARS {
        message.to_string()
    } else {
        format!("{}...", message.chars().take(MAX_CHARS).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_rejects_credentials_and_query_data() {
        assert!(matches!(
            HttpScorpioFsClient::new("http://user:secret@localhost:2725/antares"),
            Err(BackendError::InvalidEndpoint(_))
        ));
        assert!(matches!(
            HttpScorpioFsClient::new("http://localhost:2725/antares?token=secret"),
            Err(BackendError::InvalidEndpoint(_))
        ));
    }

    #[test]
    fn endpoint_appends_antares_routes_without_replacing_prefix() {
        let client =
            HttpScorpioFsClient::new("http://127.0.0.1:2725/antares/").expect("valid endpoint");
        let url = client
            .url(&["mounts", "mount-1", "ready"])
            .expect("valid route");
        assert_eq!(
            url.as_str(),
            "http://127.0.0.1:2725/antares/mounts/mount-1/ready"
        );
    }

    #[test]
    fn changed_paths_reject_absolute_and_parent_paths() {
        for path in ["/absolute", "../outside", "src/../outside", "src//lib.rs"] {
            let changed = ChangedPath {
                kind: ChangeKind::Modified,
                path: path.to_string(),
                source_path: None,
            };
            assert!(changed.validate().is_err(), "{path} must be rejected");
        }
    }

    #[test]
    fn backend_record_round_trips_without_credentials() {
        let endpoint = Url::parse("http://127.0.0.1:2725/antares").expect("valid URL");
        let request = MountRequest {
            job_id: "build-123".to_string(),
            path: "/project/aardvark-dns".to_string(),
            cl: Some("1XFJ4PGK".to_string()),
            base_oid: None,
        };
        let mount = MountResponse {
            mount_id: "mount-123".to_string(),
            mountpoint: "/var/lib/scorpiofs/antares/mnt/mount-123".to_string(),
            base_oid: None,
            ready: Some(false),
        };
        let record =
            ScorpioFsBackendRecord::new(&endpoint, &mount, &request).expect("valid record");
        let encoded = serde_json::to_string(&record).expect("serialize record");
        let decoded: ScorpioFsBackendRecord =
            serde_json::from_str(&encoded).expect("deserialize record");

        assert_eq!(decoded, record);
        assert!(!encoded.contains("secret"));
        decoded.validate().expect("record remains valid");
    }

    #[test]
    fn renamed_change_requires_a_source_path() {
        let change = ChangedPath {
            kind: ChangeKind::Renamed,
            path: "src/new.rs".to_string(),
            source_path: None,
        };
        assert!(matches!(
            change.validate(),
            Err(WorktreeBackendError::InvalidChangedPath(_))
        ));
    }

    #[test]
    fn change_set_candidates_include_rename_sources_and_are_deduplicated() {
        let changes = ChangeSet {
            mount_id: "mount-1".to_string(),
            generation: 1,
            changes: vec![
                ChangedPath {
                    kind: ChangeKind::Modified,
                    path: "src/new.rs".to_string(),
                    source_path: None,
                },
                ChangedPath {
                    kind: ChangeKind::Renamed,
                    path: "src/new.rs".to_string(),
                    source_path: Some("src/old.rs".to_string()),
                },
            ],
        };

        assert_eq!(
            changes.candidate_paths(),
            vec![PathBuf::from("src/new.rs"), PathBuf::from("src/old.rs")]
        );
    }

    #[test]
    fn libra_state_owns_mount_lifecycle_transitions() {
        let temp = tempfile::tempdir().expect("temporary state root");
        let request = MountRequest {
            job_id: "build-123".to_string(),
            path: "/project/aardvark-dns".to_string(),
            cl: None,
            base_oid: None,
        };
        let mount = MountResponse {
            mount_id: "mount-123".to_string(),
            mountpoint: "/mnt/aardvark-dns".to_string(),
            base_oid: None,
            ready: Some(true),
        };

        let mut state = LibraScorpioFsState::default();
        state
            .begin_mount(
                request,
                BackendTransport::ManagedCrate,
                "http://127.0.0.1:2725".to_string(),
            )
            .expect("begin mount");
        assert_eq!(
            state.mounts["build-123"].lifecycle,
            BackendLifecycle::Mounting
        );
        state
            .mark_ready("build-123", &mount, "scorpiofs-worktree")
            .expect("mark ready");
        state.save(temp.path()).expect("save state");

        let loaded = LibraScorpioFsState::load(temp.path()).expect("load state");
        let desired = &loaded.mounts["build-123"];
        assert_eq!(desired.lifecycle, BackendLifecycle::Ready);
        assert_eq!(desired.transport, BackendTransport::ManagedCrate);
        assert_eq!(desired.mount_id.as_deref(), Some("mount-123"));
        assert_eq!(desired.worktree_id.as_deref(), Some("scorpiofs-worktree"));
    }

    #[test]
    fn libra_state_lock_serializes_updates() {
        let temp = tempfile::tempdir().unwrap();
        let first = ScorpioFsStateLock::acquire(temp.path()).unwrap();
        let storage = temp.path().to_path_buf();
        let acquired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let acquired_in_thread = acquired.clone();

        let waiter = std::thread::spawn(move || {
            let _second = ScorpioFsStateLock::acquire(&storage).unwrap();
            acquired_in_thread.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        std::thread::sleep(Duration::from_millis(50));
        assert!(!acquired.load(std::sync::atomic::Ordering::SeqCst));
        drop(first);
        waiter.join().unwrap();
        assert!(acquired.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn libra_state_rejects_reusing_a_job_for_another_path() {
        let mut state = LibraScorpioFsState::default();
        state
            .begin_mount(
                MountRequest {
                    job_id: "build-123".to_string(),
                    path: "/project/a".to_string(),
                    cl: None,
                    base_oid: None,
                },
                BackendTransport::ManagedCrate,
                "http://127.0.0.1:2725".to_string(),
            )
            .expect("first mount");

        assert!(matches!(
            state.begin_mount(
                MountRequest {
                    job_id: "build-123".to_string(),
                    path: "/project/b".to_string(),
                    cl: None,
                    base_oid: None,
                },
                BackendTransport::ManagedCrate,
                "http://127.0.0.1:2725".to_string(),
            ),
            Err(BackendError::InvalidRecord(_))
        ));
    }
}
