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
/// any terminal or HTTP representation so all adapters share one lease owner.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ControllerKind {
    #[default]
    None,
    Browser,
    Automation,
    /// Historical local interactive owner. This preserves the `"tui"` wire
    /// value for old snapshots while remaining invalid for new leases.
    #[serde(rename = "tui")]
    LegacyLocal,
    Cli,
}

impl ControllerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Browser => "browser",
            Self::Automation => "automation",
            Self::LegacyLocal => "tui",
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
    active_lease: Option<ControllerLease>,
    /// True once any remote lease has been minted. Subsequent attaches after
    /// expiry/reclaim/detach are takeovers (W4-13).
    ever_had_remote_lease: bool,
    /// Set when a remote lease expires so callers can drop session memos
    /// before local control resumes (W4-13).
    pending_approval_invalidation: bool,
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
                "Automation write control is not enabled; start with --control write".to_string()
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
        let fixed = match options.initial_controller {
            ControllerInitial::Unclaimed => None,
            ControllerInitial::Fixed {
                kind,
                owner_label,
                reason,
            } => Some(FixedController {
                kind,
                owner_label,
                reason,
            }),
        };

        Self {
            state: Arc::new(Mutex::new(ControllerRuntimeState {
                fixed,
                active_lease: None,
                ever_had_remote_lease: false,
                pending_approval_invalidation: false,
            })),
            browser_write_enabled: options.browser_write_enabled,
            automation_write_enabled: options.automation_write_enabled,
            lease_duration: options.lease_duration,
        }
    }

    /// Move the active lease into the past without relying on wall-clock
    /// sleeps. Unit tests use this to exercise expiry deterministically even
    /// when the test runner is heavily loaded.
    #[cfg(any(test, feature = "test-provider"))]
    pub(crate) async fn expire_active_lease_for_test(&self) -> bool {
        let mut state = self.state.lock().await;
        let Some(lease) = state.active_lease.as_mut() else {
            return false;
        };
        lease.expires_at = Utc::now() - Duration::milliseconds(1);
        true
    }

    /// Admit or renew a remote controller.  A different client can only attach
    /// after the old lease expires or is explicitly detached.
    pub async fn attach(
        &self,
        kind: ControllerKind,
        client_id: &str,
    ) -> Result<ControllerLease, ControllerServiceError> {
        Ok(self.attach_with_takeover(kind, client_id).await?.0)
    }

    /// Like [`Self::attach`], but reports whether this mint is a lease
    /// takeover: a prior remote lease expired, was reclaimed, or was detached.
    /// Renewals of the same
    /// client are not takeovers.
    pub async fn attach_with_takeover(
        &self,
        kind: ControllerKind,
        client_id: &str,
    ) -> Result<(ControllerLease, bool), ControllerServiceError> {
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
            return Ok((existing.clone(), false));
        }

        let is_takeover = state.ever_had_remote_lease;
        let lease = ControllerLease {
            kind,
            client_id: client_id.to_string(),
            token: Uuid::new_v4().to_string(),
            fence: Uuid::new_v4().to_string(),
            expires_at: Utc::now() + self.lease_duration,
        };
        state.active_lease = Some(lease.clone());
        state.ever_had_remote_lease = true;
        Ok((lease, is_takeover))
    }

    /// Release a lease.  A no-longer-active lease is already released and is
    /// deliberately idempotent; a mismatched active lease fails closed.
    ///
    /// Returns whether an active lease was actually released so callers can
    /// drop prior-controller session approvals.
    pub async fn detach(
        &self,
        kind: ControllerKind,
        client_id: &str,
        token: &str,
    ) -> Result<bool, ControllerServiceError> {
        let mut state = self.state.lock().await;
        Self::clear_expired_lease(&mut state, Utc::now());
        let Some(existing) = state.active_lease.as_ref() else {
            return Ok(false);
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
        Ok(true)
    }

    /// Expire a stale remote lease (if any) and take the one-shot approval
    /// invalidation signal. Callers must drop session/TTL memos when this
    /// returns true so local control cannot reuse the prior controller's
    /// approvals.
    pub async fn take_pending_approval_invalidation(&self) -> bool {
        let mut state = self.state.lock().await;
        Self::clear_expired_lease(&mut state, Utc::now());
        let pending = state.pending_approval_invalidation;
        state.pending_approval_invalidation = false;
        pending
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

    /// Process-level shutdown release. Clears any active remote/local lease so
    /// a restarted controller cannot reuse a fence from this process. Idempotent.
    pub async fn release_for_process_shutdown(&self) {
        let mut state = self.state.lock().await;
        state.active_lease = None;
    }

    fn clear_expired_lease(state: &mut ControllerRuntimeState, now: DateTime<Utc>) -> bool {
        if state
            .active_lease
            .as_ref()
            .is_some_and(|lease| lease.expires_at <= now)
        {
            state.active_lease = None;
            state.pending_approval_invalidation = true;
            return true;
        }
        false
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
    async fn permit_fences_detach_until_dispatch_completes() {
        // W5-02 refit: the deleted local-TUI reclaim was the original
        // contested mutation; the controller's own DETACH exercises the
        // same fence (the permit holds the controller-state lock, so no
        // attach/detach mutation may interleave with an in-flight
        // authorized dispatch).
        let service = ControllerService::new(ControllerServiceOptions::new(
            true,
            true,
            ControllerInitial::Unclaimed,
        ));
        let lease = service
            .attach(ControllerKind::Browser, "browser-a")
            .await
            .expect("browser controller should attach");
        let permit = service
            .acquire_write_permit(Some(&lease.token))
            .await
            .expect("active controller should acquire a permit");

        let service_for_detach = service.clone();
        let token = lease.token.clone();
        let detach = tokio::spawn(async move {
            service_for_detach
                .detach(ControllerKind::Browser, "browser-a", &token)
                .await
        });
        tokio::task::yield_now().await;
        assert!(
            !detach.is_finished(),
            "detach must not interleave after controller authorization"
        );
        drop(permit);
        assert!(
            detach
                .await
                .expect("detach task should join")
                .expect("controller should detach after the dispatch boundary"),
            "detach should clear the active lease"
        );

        let error = service
            .authorize_write(Some(&lease.token))
            .await
            .expect_err("detached controller must be fenced out");
        match error {
            ControllerServiceError::Conflict(_) | ControllerServiceError::InvalidToken => {}
            other => panic!("unexpected error after detach: {other:?}"),
        }
    }

    #[tokio::test]
    async fn expiry_replaces_lease_with_new_fence() {
        let service = Arc::new(ControllerService::new(ControllerServiceOptions::new(
            true,
            false,
            ControllerInitial::Unclaimed,
        )));
        let first = service
            .attach(ControllerKind::Browser, "browser-a")
            .await
            .expect("first browser controller should attach");
        assert!(service.expire_active_lease_for_test().await);
        let replacement = service
            .attach(ControllerKind::Browser, "browser-b")
            .await
            .expect("expired lease should release the controller");

        assert_ne!(first.token, replacement.token);
        assert_ne!(first.fence, replacement.fence);
        assert_eq!(replacement.client_id, "browser-b");
    }

    #[tokio::test]
    async fn first_remote_attach_from_unclaimed_is_not_takeover() {
        let service = ControllerService::new(ControllerServiceOptions::new(
            true,
            false,
            ControllerInitial::Unclaimed,
        ));
        let (_, is_takeover) = service
            .attach_with_takeover(ControllerKind::Browser, "browser-a")
            .await
            .expect("first unclaimed attach");
        assert!(
            !is_takeover,
            "unclaimed first attach has no prior controller to invalidate"
        );
    }

    #[tokio::test]
    async fn detach_and_expiry_signal_approval_invalidation() {
        let service = ControllerService::new(ControllerServiceOptions::new(
            true,
            false,
            ControllerInitial::Unclaimed,
        ));
        let lease = service
            .attach(ControllerKind::Browser, "browser-a")
            .await
            .expect("attach");
        assert!(
            !service.take_pending_approval_invalidation().await,
            "fresh lease must not invalidate"
        );

        let released = service
            .detach(ControllerKind::Browser, "browser-a", &lease.token)
            .await
            .expect("detach");
        assert!(released, "active lease must report release");

        let lease = service
            .attach(ControllerKind::Browser, "browser-b")
            .await
            .expect("reattach");
        assert!(service.expire_active_lease_for_test().await);
        assert!(
            service.take_pending_approval_invalidation().await,
            "expired lease must invalidate before local control resumes"
        );
        let _ = lease;
    }
}
