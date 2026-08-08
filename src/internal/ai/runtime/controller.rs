//! UI-neutral controller lease service for `libra code` runtimes.
//!
//! The service is the single owner of controller identity, lease expiry, and
//! fencing.  UI adapters may translate its records into wire types, but they
//! must acquire a [`ControllerWritePermit`] before dispatching a mutating
//! operation.  The permit holds the controller-state lock for the short
//! adapter dispatch, so an expired/reclaimed lease cannot be preempted between
//! authorization and that dispatch.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OwnedMutexGuard};
use uuid::Uuid;

/// Default controller-lease duration used by production `libra code` sessions.
pub const DEFAULT_CONTROLLER_LEASE_SECS: i64 = 120;

/// The class of a session controller.  This is intentionally independent from
/// any TUI or HTTP representation so all adapters share one lease owner.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ControllerKind {
    #[default]
    None,
    Browser,
    Automation,
    Tui,
    Cli,
}

impl ControllerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Browser => "browser",
            Self::Automation => "automation",
            Self::Tui => "tui",
            Self::Cli => "cli",
        }
    }
}

/// Initial non-remote ownership for a session.
#[derive(Debug, Clone)]
pub enum ControllerInitial {
    Unclaimed,
    Fixed {
        kind: ControllerKind,
        owner_label: String,
        reason: Option<String>,
    },
    LocalTui {
        owner_label: String,
        reason: Option<String>,
    },
}

#[derive(Debug)]
struct FixedController {
    kind: ControllerKind,
    owner_label: String,
    reason: Option<String>,
}

/// A currently valid controller lease.  `fence` changes whenever a new owner
/// is admitted, so downstream adapters can record an opaque generation if
/// they need to correlate a command with its controlling client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerLease {
    pub kind: ControllerKind,
    pub client_id: String,
    pub token: String,
    pub fence: String,
    pub expires_at: DateTime<Utc>,
}

/// UI-neutral projection of controller ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerSnapshot {
    pub kind: ControllerKind,
    pub owner_label: Option<String>,
    pub can_write: bool,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub reason: Option<String>,
}

#[derive(Debug)]
struct ControllerRuntimeState {
    fixed: Option<FixedController>,
    local_tui_owner: Option<FixedController>,
    active_lease: Option<ControllerLease>,
}

/// Runtime configuration for [`ControllerService`].
#[derive(Debug, Clone)]
pub struct ControllerServiceOptions {
    pub browser_write_enabled: bool,
    pub automation_write_enabled: bool,
    pub initial_controller: ControllerInitial,
    pub lease_duration: Duration,
}

impl ControllerServiceOptions {
    pub fn new(
        browser_write_enabled: bool,
        automation_write_enabled: bool,
        initial_controller: ControllerInitial,
    ) -> Self {
        Self {
            browser_write_enabled,
            automation_write_enabled,
            initial_controller,
            lease_duration: Duration::seconds(DEFAULT_CONTROLLER_LEASE_SECS),
        }
    }
}

/// Stable controller-domain failures.  The Web adapter maps these into its
/// documented HTTP/JSON error contract; the runtime itself does not depend on
/// HTTP types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerServiceError {
    BrowserControlDisabled,
    AutomationControlDisabled,
    InvalidControllerKind(ControllerKind),
    Conflict(String),
    MissingToken,
    InvalidToken,
}

impl ControllerServiceError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::BrowserControlDisabled => "BROWSER_CONTROL_DISABLED",
            Self::AutomationControlDisabled => "CONTROL_DISABLED",
            Self::InvalidControllerKind(_) => "INVALID_CONTROLLER_KIND",
            Self::Conflict(_) => "CONTROLLER_CONFLICT",
            Self::MissingToken => "MISSING_CONTROLLER_TOKEN",
            Self::InvalidToken => "INVALID_CONTROLLER_TOKEN",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::BrowserControlDisabled => {
                "Browser control is disabled for this code session".to_string()
            }
            Self::AutomationControlDisabled => {
                "Local TUI automation write control is not enabled; start with --control write"
                    .to_string()
            }
            Self::InvalidControllerKind(kind) => {
                format!("Controller kind '{}' cannot attach", kind.as_str())
            }
            Self::Conflict(message) => message.clone(),
            Self::MissingToken => "A controller token is required for write operations".to_string(),
            Self::InvalidToken => {
                "The controller token does not match the active controller".to_string()
            }
        }
    }

    pub fn http_status(&self) -> u16 {
        match self {
            Self::InvalidControllerKind(_) => 400,
            Self::Conflict(_) => 409,
            Self::BrowserControlDisabled
            | Self::AutomationControlDisabled
            | Self::MissingToken
            | Self::InvalidToken => 403,
        }
    }
}

/// A permit proving that its holder is still the active controller.
///
/// It owns the state lock until dropped.  Hold it only around the dispatch that
/// begins the adapter operation, never around unbounded model/tool execution.
/// This makes controller fencing atomic without serializing ordinary turns.
pub struct ControllerWritePermit {
    _state: OwnedMutexGuard<ControllerRuntimeState>,
    lease: ControllerLease,
}

impl ControllerWritePermit {
    pub fn lease(&self) -> &ControllerLease {
        &self.lease
    }
}

/// One session's controller owner and lease state machine.
#[derive(Clone)]
pub struct ControllerService {
    state: Arc<Mutex<ControllerRuntimeState>>,
    browser_write_enabled: bool,
    automation_write_enabled: bool,
    lease_duration: Duration,
}

impl ControllerService {
    pub fn new(options: ControllerServiceOptions) -> Self {
        let (fixed, local_tui_owner) = match options.initial_controller {
            ControllerInitial::Unclaimed => (None, None),
            ControllerInitial::Fixed {
                kind,
                owner_label,
                reason,
            } => (
                Some(FixedController {
                    kind,
                    owner_label,
                    reason,
                }),
                None,
            ),
            ControllerInitial::LocalTui {
                owner_label,
                reason,
            } => (
                None,
                Some(FixedController {
                    kind: ControllerKind::Tui,
                    owner_label,
                    reason,
                }),
            ),
        };

        Self {
            state: Arc::new(Mutex::new(ControllerRuntimeState {
                fixed,
                local_tui_owner,
                active_lease: None,
            })),
            browser_write_enabled: options.browser_write_enabled,
            automation_write_enabled: options.automation_write_enabled,
            lease_duration: options.lease_duration,
        }
    }

    /// Admit or renew a remote controller.  A different client can only attach
    /// after the old lease expires or the local TUI explicitly reclaims it.
    pub async fn attach(
        &self,
        kind: ControllerKind,
        client_id: &str,
    ) -> Result<ControllerLease, ControllerServiceError> {
        match kind {
            ControllerKind::Browser if !self.browser_write_enabled => {
                return Err(ControllerServiceError::BrowserControlDisabled);
            }
            ControllerKind::Automation if !self.automation_write_enabled => {
                return Err(ControllerServiceError::AutomationControlDisabled);
            }
            ControllerKind::Browser | ControllerKind::Automation => {}
            _ => return Err(ControllerServiceError::InvalidControllerKind(kind)),
        }

        let mut state = self.state.lock().await;
        if let Some(fixed) = state.fixed.as_ref() {
            return Err(ControllerServiceError::Conflict(format!(
                "The active controller is {} ({})",
                fixed.kind.as_str(),
                fixed.owner_label
            )));
        }
        Self::clear_expired_lease(&mut state, Utc::now());

        if let Some(existing) = state.active_lease.as_mut() {
            if existing.client_id != client_id || existing.kind != kind {
                return Err(ControllerServiceError::Conflict(format!(
                    "Another {} currently controls this session",
                    existing.kind.as_str()
                )));
            }
            existing.expires_at = Utc::now() + self.lease_duration;
            return Ok(existing.clone());
        }

        let lease = ControllerLease {
            kind,
            client_id: client_id.to_string(),
            token: Uuid::new_v4().to_string(),
            fence: Uuid::new_v4().to_string(),
            expires_at: Utc::now() + self.lease_duration,
        };
        state.active_lease = Some(lease.clone());
        Ok(lease)
    }

    /// Release a lease.  A no-longer-active lease is already released and is
    /// deliberately idempotent; a mismatched active lease fails closed.
    pub async fn detach(
        &self,
        kind: ControllerKind,
        client_id: &str,
        token: &str,
    ) -> Result<(), ControllerServiceError> {
        let mut state = self.state.lock().await;
        Self::clear_expired_lease(&mut state, Utc::now());
        let Some(existing) = state.active_lease.as_ref() else {
            return Ok(());
        };
        if existing.kind != kind {
            return Err(ControllerServiceError::Conflict(
                "The requested controller kind does not own this session".to_string(),
            ));
        }
        if existing.client_id != client_id || existing.token != token {
            return Err(ControllerServiceError::InvalidToken);
        }
        state.active_lease = None;
        Ok(())
    }

    /// Check a controller token and renew the lease without retaining the
    /// state lock.  Use [`Self::acquire_write_permit`] for an actual mutation.
    pub async fn authorize_write(
        &self,
        token: Option<&str>,
    ) -> Result<ControllerLease, ControllerServiceError> {
        let permit = self.acquire_write_permit(token).await?;
        Ok(permit.lease().clone())
    }

    /// Atomically validate/renew a lease and keep the controller state fenced
    /// until the caller has dispatched the mutation.  Dropping the returned
    /// permit admits attach, detach, and reclaim operations again.
    pub async fn acquire_write_permit(
        &self,
        token: Option<&str>,
    ) -> Result<ControllerWritePermit, ControllerServiceError> {
        let Some(token) = token.filter(|token| !token.trim().is_empty()) else {
            return Err(ControllerServiceError::MissingToken);
        };

        let mut state = self.state.clone().lock_owned().await;
        let now = Utc::now();
        Self::clear_expired_lease(&mut state, now);
        let Some(lease) = state.active_lease.as_mut() else {
            return Err(ControllerServiceError::Conflict(
                "No client currently controls this session".to_string(),
            ));
        };
        if lease.token != token {
            return Err(ControllerServiceError::InvalidToken);
        }
        lease.expires_at = now + self.lease_duration;
        let lease = lease.clone();
        Ok(ControllerWritePermit {
            _state: state,
            lease,
        })
    }

    /// Return the current projected controller state, expiring a stale remote
    /// lease before reporting it.
    pub async fn snapshot(&self) -> ControllerSnapshot {
        let mut state = self.state.lock().await;
        Self::clear_expired_lease(&mut state, Utc::now());
        Self::snapshot_from_state(&state, self.browser_write_enabled)
    }

    /// Local TUI reclaim is deliberately scoped to sessions that were started
    /// with a local TUI owner.  Other callers cannot force-clear a lease.
    pub async fn reclaim_local_tui(&self) -> Result<(), ControllerServiceError> {
        let mut state = self.state.lock().await;
        if state.local_tui_owner.is_none() {
            return Err(ControllerServiceError::Conflict(
                "This session does not have a local TUI controller to reclaim".to_string(),
            ));
        }
        state.active_lease = None;
        Ok(())
    }

    fn clear_expired_lease(state: &mut ControllerRuntimeState, now: DateTime<Utc>) {
        if state
            .active_lease
            .as_ref()
            .is_some_and(|lease| lease.expires_at <= now)
        {
            state.active_lease = None;
        }
    }

    fn snapshot_from_state(
        state: &ControllerRuntimeState,
        browser_write_enabled: bool,
    ) -> ControllerSnapshot {
        if let Some(lease) = state.active_lease.as_ref() {
            return ControllerSnapshot {
                kind: lease.kind,
                owner_label: Some(lease.client_id.clone()),
                can_write: true,
                lease_expires_at: Some(lease.expires_at),
                reason: None,
            };
        }
        if let Some(local) = state.local_tui_owner.as_ref() {
            return ControllerSnapshot {
                kind: local.kind,
                owner_label: Some(local.owner_label.clone()),
                can_write: false,
                lease_expires_at: None,
                reason: local.reason.clone(),
            };
        }
        if let Some(fixed) = state.fixed.as_ref() {
            return ControllerSnapshot {
                kind: fixed.kind,
                owner_label: Some(fixed.owner_label.clone()),
                can_write: false,
                lease_expires_at: None,
                reason: fixed.reason.clone(),
            };
        }
        ControllerSnapshot {
            kind: ControllerKind::None,
            owner_label: None,
            can_write: false,
            lease_expires_at: None,
            reason: if browser_write_enabled {
                Some("No controller attached".to_string())
            } else {
                Some("Browser control is disabled".to_string())
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn permit_fences_reclaim_until_dispatch_completes() {
        let service = ControllerService::new(ControllerServiceOptions::new(
            false,
            true,
            ControllerInitial::LocalTui {
                owner_label: "TUI".to_string(),
                reason: None,
            },
        ));
        let lease = service
            .attach(ControllerKind::Automation, "automation-a")
            .await
            .expect("automation controller should attach");
        let permit = service
            .acquire_write_permit(Some(&lease.token))
            .await
            .expect("active controller should acquire a permit");

        let service_for_reclaim = service.clone();
        let reclaim = tokio::spawn(async move { service_for_reclaim.reclaim_local_tui().await });
        tokio::task::yield_now().await;
        assert!(
            !reclaim.is_finished(),
            "reclaim must not interleave after controller authorization"
        );
        drop(permit);
        reclaim
            .await
            .expect("reclaim task should join")
            .expect("local TUI should reclaim after dispatch boundary");

        let error = service
            .authorize_write(Some(&lease.token))
            .await
            .expect_err("reclaimed controller must be fenced out");
        assert_eq!(
            error,
            ControllerServiceError::Conflict(
                "No client currently controls this session".to_string()
            )
        );
    }

    #[tokio::test]
    async fn expiry_replaces_lease_with_new_fence() {
        let mut options = ControllerServiceOptions::new(true, false, ControllerInitial::Unclaimed);
        options.lease_duration = Duration::milliseconds(1);
        let service = Arc::new(ControllerService::new(options));
        let first = service
            .attach(ControllerKind::Browser, "browser-a")
            .await
            .expect("first browser controller should attach");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let replacement = service
            .attach(ControllerKind::Browser, "browser-b")
            .await
            .expect("expired lease should release the controller");

        assert_ne!(first.token, replacement.token);
        assert_ne!(first.fence, replacement.fence);
        assert_eq!(replacement.client_id, "browser-b");
    }
}
