//! Sandbox structured-event sink — OC-Phase 7 P2 Evidence wiring.
//!
//! Per `docs/development/commands/sandbox.md` lines 142-144, 162, 348, and 373:
//! sandbox rejection events, cleanup failures, dangerous writable
//! roots, and (future) enforcement / network denials must surface as
//! structured records the agent runtime can route to
//! `ToolInvocation[E]` / `Evidence[E]` (see `agent.md` Part B).
//! Today the sandbox emits `tracing::warn!` only — observable in a
//! local console but invisible to the audit layer.
//!
//! This module introduces a minimal trait + variant set so the
//! sandbox can call a single `record(event)` hook at every doc-named
//! surface; the default implementation keeps the current
//! tracing-warn behaviour so callers that do not opt in see no
//! change. Future agent-runtime work plugs in a sink that fans out
//! to `AgentEvidence` rows (per
//! [`crate::internal::ai::agent_run::evidence::AgentEvidence`]) and
//! eventually to the persistent `git_internal::Evidence` snapshot.
//!
//! # Scope
//!
//! The sink is intentionally narrow — it carries only the event
//! variants the sandbox itself can produce. Higher-level agent
//! Evidence (claim shapes, criterion ids, AgentRunId) belongs to
//! the agent-runtime layer and must be assembled by the sink
//! implementation, not by the sandbox.

use std::{
    fmt::Debug,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use super::NetworkProtocol;

/// Structured sandbox events that callers can observe via
/// [`SandboxEvidenceSink::record`].
///
/// Each variant maps 1:1 to a previously-tracing-only surface in
/// the sandbox. New variants must be additive — older sink
/// implementations should ignore unknown variants gracefully (the
/// default trait method runs the variant through the trait's
/// match exhaustively, so no silent drop on the read side, but
/// implementors are free to match-anything for unknown future
/// variants).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SandboxEvidenceEvent {
    /// `cleanup_command_tmpdir(path)` failed to remove the
    /// per-command private tmp directory. Doc reference:
    /// `docs/development/commands/sandbox.md:142`.
    TmpdirCleanupFailed {
        /// Tmp directory the sandbox was attempting to remove.
        path: PathBuf,
        /// Stringified [`std::io::Error`] from the underlying
        /// `tokio::fs::remove_dir_all` call. Carried as a `String`
        /// so the variant stays `Clone`/`PartialEq`/`Eq` and the
        /// sink implementations do not need to depend on the
        /// concrete error type.
        error: String,
    },
    /// `SandboxPolicy::validate_writable_roots_with_cwd` refused a
    /// configured `writable_root` (e.g. `/`, `/proc`, a docker
    /// socket path). Doc reference: `docs/development/commands/sandbox.md:143`.
    WritableRootRejected {
        /// The rejected writable root, resolved against the
        /// command's working directory.
        root: PathBuf,
        /// Static reason string from
        /// [`crate::internal::ai::sandbox::policy::SandboxPolicyError::DangerousWritableRoot`].
        reason: String,
    },
    /// `SandboxManager::transform` refused to build a command because
    /// `SandboxEnforcement::Required` (or `PreferStrict` after a
    /// failed downgrade prompt) demanded an effective sandbox the
    /// host could not provide — typically a missing
    /// `LIBRA_LINUX_SANDBOX_EXE` helper on Linux, or an unsupported
    /// platform / policy combination elsewhere. Doc reference:
    /// `docs/development/commands/sandbox.md:143`, L162, L373 ("enforcement
    /// 失败 必须落到 ... `Evidence[E]`，不能藏在 tracing 日志里").
    EnforcementFailed {
        /// Verbatim `reason` string from
        /// [`crate::internal::ai::sandbox::runtime::SandboxTransformError::EnforcementFailed`].
        /// Carried as `String` so sink implementations can route
        /// the reason to audit storage without depending on the
        /// concrete error type.
        reason: String,
    },
    /// `SandboxManager::transform` refused to build a command
    /// because the network enforcement layer rejected the
    /// configured policy — typically `NetworkAccess::Allowlist`
    /// requested under `SandboxEnforcement::Required` while the
    /// per-allowlist proxy backend is unavailable. Doc reference:
    /// `docs/development/commands/sandbox.md:348` ("网络拒绝事件（连接被
    /// OS 或代理阻断）写入 `ToolInvocation[E] + Evidence[E]`) and
    /// L373.
    ///
    /// The runtime's `SandboxTransformError::NetworkEnforcementFailed`
    /// is the dual of `EnforcementFailed` for the network axis;
    /// pre-positioning the Evidence variant lets Phase 7's real
    /// proxy backend emit structured audit signals from day one
    /// instead of starting on `tracing::warn!` like the other
    /// variants did pre-v0.17.720.
    NetworkEnforcementFailed {
        /// Verbatim `reason` string from
        /// [`crate::internal::ai::sandbox::runtime::SandboxTransformError::NetworkEnforcementFailed`].
        reason: String,
    },
    /// The runtime allowlist proxy denied an individual outbound
    /// connection request after inspecting the CONNECT target or HTTP
    /// Host header. Doc reference: `docs/development/commands/sandbox.md:348`
    /// ("网络拒绝事件（连接被 OS 或代理阻断）写入
    /// `ToolInvocation[E] + Evidence[E]`).
    NetworkRequestDenied {
        /// Stable proxy backend identifier, e.g. `"allowlist"`.
        proxy_backend: String,
        /// Requested destination host.
        host: String,
        /// Requested destination port.
        port: u16,
        /// Requested wire protocol.
        protocol: NetworkProtocol,
        /// Human-readable denial reason from the proxy decision.
        reason: String,
    },
}

impl SandboxEvidenceEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::TmpdirCleanupFailed { .. } => "tmpdir_cleanup_failed",
            Self::WritableRootRejected { .. } => "writable_root_rejected",
            Self::EnforcementFailed { .. } => "enforcement_failed",
            Self::NetworkEnforcementFailed { .. } => "network_enforcement_failed",
            Self::NetworkRequestDenied { .. } => "network_request_denied",
        }
    }

    pub fn to_metadata_value(&self) -> serde_json::Value {
        match self {
            Self::TmpdirCleanupFailed { path, error } => serde_json::json!({
                "kind": self.kind(),
                "path": path.display().to_string(),
                "error": error,
            }),
            Self::WritableRootRejected { root, reason } => serde_json::json!({
                "kind": self.kind(),
                "root": root.display().to_string(),
                "reason": reason,
            }),
            Self::EnforcementFailed { reason } | Self::NetworkEnforcementFailed { reason } => {
                serde_json::json!({
                    "kind": self.kind(),
                    "reason": reason,
                })
            }
            Self::NetworkRequestDenied {
                proxy_backend,
                host,
                port,
                protocol,
                reason,
            } => serde_json::json!({
                "kind": self.kind(),
                "proxy_backend": proxy_backend,
                "host": host,
                "port": port,
                "protocol": protocol,
                "reason": reason,
            }),
        }
    }
}

/// Object-safe sink the sandbox calls at each structured event
/// surface. Implementations MUST be cheap to call — they sit on
/// the hot path of every sandboxed command — and MUST NOT panic.
///
/// Threading: the sink is shared across the tokio runtime (the
/// `cleanup_command_tmpdir` call site runs in a tokio task), so
/// implementations must be `Send + Sync`. `Debug` is a
/// super-trait so [`crate::internal::ai::sandbox::SandboxRuntimeConfig`]
/// can keep its derived `#[derive(Debug)]`.
pub trait SandboxEvidenceSink: Debug + Send + Sync {
    fn record(&self, event: SandboxEvidenceEvent);
}

/// Sink that forwards every event to multiple child sinks.
///
/// Used by the shell runtime to capture structured metadata while preserving
/// an already-configured runtime sink or the default tracing fallback.
#[derive(Debug)]
pub struct FanoutSandboxEvidenceSink {
    sinks: Vec<Arc<dyn SandboxEvidenceSink>>,
}

impl FanoutSandboxEvidenceSink {
    pub fn new(sinks: Vec<Arc<dyn SandboxEvidenceSink>>) -> Self {
        Self { sinks }
    }
}

impl SandboxEvidenceSink for FanoutSandboxEvidenceSink {
    fn record(&self, event: SandboxEvidenceEvent) {
        for sink in &self.sinks {
            sink.record(event.clone());
        }
    }
}

/// Default sink that mirrors the pre-Phase-7 behaviour:
/// emit a `tracing::warn!` with the structured fields so existing
/// log scrapers keep working without code change. Plugged in by
/// the sandbox runtime when no explicit sink is configured.
#[derive(Clone, Copy, Debug, Default)]
pub struct TracingSandboxEvidenceSink;

impl SandboxEvidenceSink for TracingSandboxEvidenceSink {
    fn record(&self, event: SandboxEvidenceEvent) {
        match event {
            SandboxEvidenceEvent::TmpdirCleanupFailed { path, error } => {
                tracing::warn!(
                    path = %path.display(),
                    error = %error,
                    "sandbox.evidence tmpdir_cleanup_failed",
                );
            }
            SandboxEvidenceEvent::WritableRootRejected { root, reason } => {
                tracing::warn!(
                    root = %root.display(),
                    reason = %reason,
                    "sandbox.evidence writable_root_rejected",
                );
            }
            SandboxEvidenceEvent::EnforcementFailed { reason } => {
                tracing::warn!(
                    reason = %reason,
                    "sandbox.evidence enforcement_failed",
                );
            }
            SandboxEvidenceEvent::NetworkEnforcementFailed { reason } => {
                tracing::warn!(
                    reason = %reason,
                    "sandbox.evidence network_enforcement_failed",
                );
            }
            SandboxEvidenceEvent::NetworkRequestDenied {
                proxy_backend,
                host,
                port,
                protocol,
                reason,
            } => {
                tracing::warn!(
                    proxy_backend = %proxy_backend,
                    host = %host,
                    port = port,
                    protocol = ?protocol,
                    reason = %reason,
                    "sandbox.evidence network_request_denied",
                );
            }
        }
    }
}

/// In-memory sink used by tests. Stores every recorded event in
/// the order it was observed so a test can assert the sandbox
/// emitted the expected structured signal at the doc-named
/// surface (rather than only the underlying tracing line).
#[derive(Debug, Default)]
pub struct InMemorySandboxEvidenceSink {
    events: Mutex<Vec<SandboxEvidenceEvent>>,
}

impl InMemorySandboxEvidenceSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the events captured so far. Cloned out of the
    /// internal lock to keep the sink shareable while a test
    /// inspects what landed.
    pub fn events(&self) -> Vec<SandboxEvidenceEvent> {
        self.events
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }
}

impl SandboxEvidenceSink for InMemorySandboxEvidenceSink {
    fn record(&self, event: SandboxEvidenceEvent) {
        if let Ok(mut guard) = self.events.lock() {
            guard.push(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// `TracingSandboxEvidenceSink::record` is infallible across
    /// every variant — must not panic on any payload shape. Pinning
    /// the no-panic contract guards a future variant whose Display
    /// or formatting code could otherwise blow up under
    /// tracing's structured-field formatter.
    #[test]
    fn tracing_sink_records_every_variant_without_panicking() {
        let sink = TracingSandboxEvidenceSink;
        sink.record(SandboxEvidenceEvent::TmpdirCleanupFailed {
            path: PathBuf::from("/tmp/libra-sandbox-abc"),
            error: "permission denied".to_string(),
        });
        sink.record(SandboxEvidenceEvent::WritableRootRejected {
            root: PathBuf::from("/var/run/docker.sock"),
            reason: "matches dangerous mount pattern".to_string(),
        });
        sink.record(SandboxEvidenceEvent::EnforcementFailed {
            reason: "Linux sandbox enforcement is required, but LIBRA_LINUX_SANDBOX_EXE is not configured".to_string(),
        });
        sink.record(SandboxEvidenceEvent::NetworkEnforcementFailed {
            reason: "allowlist proxy unavailable in Required mode".to_string(),
        });
        sink.record(SandboxEvidenceEvent::NetworkRequestDenied {
            proxy_backend: "allowlist".to_string(),
            host: "denied.example".to_string(),
            port: 443,
            protocol: NetworkProtocol::Tcp,
            reason: "not in allowlist".to_string(),
        });
    }

    /// `InMemorySandboxEvidenceSink` captures events in the order
    /// the sandbox emits them. Pins the FIFO contract so a test
    /// asserting "tmpdir cleanup failure came after writable root
    /// rejection" remains meaningful. Also covers the
    /// `EnforcementFailed` variant so future drift in the
    /// enforcement wiring is loud.
    #[test]
    fn in_memory_sink_preserves_record_order() {
        let sink = InMemorySandboxEvidenceSink::new();
        sink.record(SandboxEvidenceEvent::TmpdirCleanupFailed {
            path: PathBuf::from("/tmp/a"),
            error: "io-1".to_string(),
        });
        sink.record(SandboxEvidenceEvent::WritableRootRejected {
            root: PathBuf::from("/dev"),
            reason: "device root".to_string(),
        });
        sink.record(SandboxEvidenceEvent::EnforcementFailed {
            reason: "missing helper".to_string(),
        });
        sink.record(SandboxEvidenceEvent::NetworkEnforcementFailed {
            reason: "proxy unavailable".to_string(),
        });
        sink.record(SandboxEvidenceEvent::NetworkRequestDenied {
            proxy_backend: "allowlist".to_string(),
            host: "denied.example".to_string(),
            port: 443,
            protocol: NetworkProtocol::Tcp,
            reason: "not in allowlist".to_string(),
        });
        let recorded = sink.events();
        assert_eq!(recorded.len(), 5);
        assert!(matches!(
            &recorded[0],
            SandboxEvidenceEvent::TmpdirCleanupFailed { path, .. }
                if path == &PathBuf::from("/tmp/a")
        ));
        assert!(matches!(
            &recorded[1],
            SandboxEvidenceEvent::WritableRootRejected { root, .. }
                if root == &PathBuf::from("/dev")
        ));
        assert!(matches!(
            &recorded[2],
            SandboxEvidenceEvent::EnforcementFailed { reason }
                if reason == "missing helper"
        ));
        assert!(matches!(
            &recorded[3],
            SandboxEvidenceEvent::NetworkEnforcementFailed { reason }
                if reason == "proxy unavailable"
        ));
        assert!(matches!(
            &recorded[4],
            SandboxEvidenceEvent::NetworkRequestDenied { host, port, .. }
                if host == "denied.example" && *port == 443
        ));
    }

    #[test]
    fn fanout_sink_forwards_to_all_children() {
        let first = Arc::new(InMemorySandboxEvidenceSink::new());
        let second = Arc::new(InMemorySandboxEvidenceSink::new());
        let fanout = FanoutSandboxEvidenceSink::new(vec![first.clone(), second.clone()]);

        fanout.record(SandboxEvidenceEvent::EnforcementFailed {
            reason: "missing helper".to_string(),
        });

        assert_eq!(first.events().len(), 1);
        assert_eq!(second.events().len(), 1);
    }

    #[test]
    fn event_metadata_uses_stable_kind_and_fields() {
        let event = SandboxEvidenceEvent::WritableRootRejected {
            root: PathBuf::from("/"),
            reason: "dangerous root".to_string(),
        };

        assert_eq!(event.kind(), "writable_root_rejected");
        assert_eq!(
            event.to_metadata_value(),
            serde_json::json!({
                "kind": "writable_root_rejected",
                "root": "/",
                "reason": "dangerous root",
            })
        );

        let event = SandboxEvidenceEvent::NetworkRequestDenied {
            proxy_backend: "allowlist".to_string(),
            host: "denied.example".to_string(),
            port: 443,
            protocol: NetworkProtocol::Tcp,
            reason: "not in allowlist".to_string(),
        };
        assert_eq!(event.kind(), "network_request_denied");
        assert_eq!(
            event.to_metadata_value(),
            serde_json::json!({
                "kind": "network_request_denied",
                "proxy_backend": "allowlist",
                "host": "denied.example",
                "port": 443,
                "protocol": "tcp",
                "reason": "not in allowlist",
            })
        );
    }

    /// The sink trait is object-safe — the sandbox stores it as
    /// `Arc<dyn SandboxEvidenceSink>` so the concrete type is
    /// erased at the boundary. Pin object-safety by constructing
    /// an `Arc<dyn ...>` and calling `record` through the trait
    /// object.
    #[test]
    fn sink_trait_is_object_safe() {
        let sink: Arc<dyn SandboxEvidenceSink> = Arc::new(InMemorySandboxEvidenceSink::new());
        sink.record(SandboxEvidenceEvent::TmpdirCleanupFailed {
            path: PathBuf::from("/tmp/x"),
            error: "io-2".to_string(),
        });
        // Down-cast back to assert capture went through.
        let inspected = sink.clone();
        // `Arc<dyn ...>` doesn't support downcast without
        // additional traits; just sanity-check via a second
        // `record` and trust the InMemorySink capture path that
        // the prior test already pinned.
        inspected.record(SandboxEvidenceEvent::WritableRootRejected {
            root: PathBuf::from("/"),
            reason: "root".to_string(),
        });
    }
}
