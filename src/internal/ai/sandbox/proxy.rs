//! Network allowlist decision layer for sandbox Phase 7
//! (`docs/development/commands/sandbox.md` §7.4).
//!
//! AI shell tools that need allowlisted outbound network access route through
//! a short-lived local HTTP/CONNECT proxy in [`super::proxy_runtime`]. This
//! module keeps the policy decision surface pure and testable:
//!
//! 1. The sandbox runtime can route through a `&dyn NetworkProxy` without
//!    depending on the transport implementation.
//! 2. Tests and the `Denied`-mode fallback can use [`NoopProxy`] (deny
//!    everything) without opening sockets.
//! 3. [`AllowlistProxy`] evaluates host / port / protocol rules used by the
//!    runtime proxy before any TCP forwarding happens.
//!
//! All implementations are `dyn`-safe and implement `Debug` so the
//! sandbox runtime can reuse one diagnostic envelope type for all
//! backends.

use std::net::IpAddr;

use super::{NetworkAccess, NetworkProtocol, NetworkService, SandboxPolicy};

/// One outbound connection request handed to a [`NetworkProxy`]
/// before the sandbox runtime forwards it.
///
/// Kept as plain owned strings / numbers so the type can cross the
/// proxy boundary without forcing every caller to depend on `url` /
/// `hyper`. The Phase 7.4 real proxy will rebuild whatever wire-type
/// it needs from these fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkRequest {
    /// Destination hostname (FQDN or IP literal). Never empty —
    /// callers that have only an `IpAddr` should stringify it before
    /// constructing the request so the proxy can apply hostname
    /// allowlist rules consistently.
    pub host: String,
    /// Destination port. Always set — the proxy needs a concrete
    /// port to apply per-port allowlist rules from
    /// [`crate::internal::ai::sandbox::policy::NetworkService`].
    pub port: u16,
    /// Wire protocol selector; matches the
    /// [`NetworkProtocol`] enum so the proxy can route TCP vs UDP
    /// without a second translation step.
    pub protocol: NetworkProtocol,
}

/// Decision returned by [`NetworkProxy::evaluate`].
///
/// Modeled as an explicit enum rather than a `bool` so the future
/// audit pipeline can pin per-decision reason codes without changing
/// the trait signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetworkDecision {
    /// The proxy allows the connection to proceed.
    Allow,
    /// The proxy denies the connection. The string is a short
    /// human-readable reason that the sandbox audit path will copy
    /// into the `ToolInvocation[E]` evidence record.
    Deny(String),
}

impl NetworkDecision {
    /// `true` when the decision allows the connection.
    pub fn is_allow(&self) -> bool {
        matches!(self, Self::Allow)
    }

    /// `true` when the decision denies the connection.
    pub fn is_deny(&self) -> bool {
        matches!(self, Self::Deny(_))
    }
}

/// Pluggable network filter that sits between the sandbox runtime
/// and the outbound transport.
///
/// The runtime proxy feeds CONNECT targets and HTTP Host headers into this
/// trait before forwarding. Keeping the decision pure lets tests cover
/// matching and denial without needing a network listener.
///
/// Implementors must be `Send + Sync + Debug` so the sandbox
/// runtime can share a single instance across worker threads via
/// `&'a dyn NetworkProxy`, and so [`NetworkProxySelection`]
/// (and other diagnostic envelopes that carry a proxy reference)
/// can derive `Debug` without bespoke impls per backend.
pub trait NetworkProxy: Send + Sync + std::fmt::Debug {
    /// Stable identifier for this proxy implementation. Used by
    /// `libra sandbox status` to surface which proxy is wired up;
    /// also serialised into the audit record so downstream readers
    /// can correlate decisions with the active backend.
    fn backend_name(&self) -> &'static str;

    /// Apply the proxy's allowlist policy to the supplied request.
    ///
    /// Returning [`NetworkDecision::Deny`] does NOT mean the proxy
    /// drops the connection on the wire — the caller (sandbox
    /// runtime) is responsible for treating the deny as a tool
    /// failure and surfacing the reason. The trait is pure-function
    /// so it stays testable without any I/O.
    fn evaluate(&self, request: &NetworkRequest) -> NetworkDecision;
}

/// Proxy implementation that denies every connection.
///
/// Used by the `Denied` mode of the upcoming
/// `NetworkAccess::{Denied, Allowlist, Full}` migration (sandbox.md
/// §7.1) — when the user has configured "no network", routing
/// through a `NoopProxy` keeps the sandbox runtime's dispatch path
/// uniform across the three modes.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopProxy;

impl NetworkProxy for NoopProxy {
    fn backend_name(&self) -> &'static str {
        "noop"
    }

    fn evaluate(&self, _request: &NetworkRequest) -> NetworkDecision {
        NetworkDecision::Deny(
            "NetworkAccess::Denied — no network access is permitted in this sandbox".to_string(),
        )
    }
}

/// Proxy implementation that allows only loopback destinations
/// (127.0.0.1 / ::1 / `localhost`).
///
/// Used by the `Full` compatibility arm until a dedicated pass-through proxy
/// type exists. Any non-loopback destination is denied with a reason naming
/// the observed host so audit consumers can see exactly which target was
/// rejected.
#[derive(Debug, Default, Clone, Copy)]
pub struct LoopbackOnlyProxy;

impl NetworkProxy for LoopbackOnlyProxy {
    fn backend_name(&self) -> &'static str {
        "loopback-only"
    }

    fn evaluate(&self, request: &NetworkRequest) -> NetworkDecision {
        if is_loopback_host(&request.host) {
            NetworkDecision::Allow
        } else {
            NetworkDecision::Deny(format!(
                "LoopbackOnlyProxy: outbound connection to {}:{} ({:?}) is not loopback",
                request.host, request.port, request.protocol,
            ))
        }
    }
}

/// Proxy implementation that enforces configured host / port / protocol
/// allowlist entries.
///
/// The transport runtime calls this before DNS / socket forwarding. A request
/// that does not match any allowlist entry is explicitly denied.
#[derive(Debug, Clone)]
pub struct AllowlistProxy {
    services: Vec<NetworkService>,
}

impl AllowlistProxy {
    pub fn new(services: &[NetworkService]) -> Self {
        Self {
            services: services.to_vec(),
        }
    }

    pub fn services(&self) -> &[NetworkService] {
        self.services.as_slice()
    }
}

impl NetworkProxy for AllowlistProxy {
    fn backend_name(&self) -> &'static str {
        "allowlist"
    }

    fn evaluate(&self, request: &NetworkRequest) -> NetworkDecision {
        for service in &self.services {
            let host_match = host_matches_service(&request.host, &service.host);
            let protocol_match = service.effective_protocol() == request.protocol;
            let port_match = service.ports.is_empty() || service.ports.contains(&request.port);

            if host_match && protocol_match && port_match {
                return NetworkDecision::Allow;
            }
        }

        NetworkDecision::Deny(format!(
            "AllowlistProxy: outbound connection to {}:{} ({:?}) is not in allowlist",
            request.host, request.port, request.protocol,
        ))
    }
}

/// `true` when `host` is the literal string `"localhost"` (case-
/// insensitive) or parses as a loopback `IpAddr`.
///
/// Kept as a free function so the same predicate can be reused by the
/// Phase 7.4 full proxy's allowlist-fallback path.
pub fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>()
        .map(|addr| addr.is_loopback())
        .unwrap_or(false)
}

fn host_matches_service(request_host: &str, configured_host: &str) -> bool {
    let request_host = request_host.to_ascii_lowercase();
    let configured_host = configured_host.to_ascii_lowercase();

    if let Some(suffix) = configured_host.strip_prefix("*.") {
        request_host.ends_with(suffix)
            && request_host.len() > suffix.len()
            && request_host.as_bytes()[request_host.len() - suffix.len() - 1] == b'.'
    } else {
        request_host == configured_host
    }
}

/// Build an [`AllowlistProxy`] from sandbox policy when the policy is
/// `NetworkAccess::Allowlist` and the configured services are usable.
///
/// `Ok(None)` is returned for non-allowlist policies. `Err` is returned when
/// allowlist proxy construction is requested but not possible, so callers can
/// surface actionable diagnostics (invalid services, empty service lists, etc.).
pub fn allowlist_proxy_from_policy(
    policy: &SandboxPolicy,
) -> Result<Option<AllowlistProxy>, String> {
    let services = match policy {
        SandboxPolicy::ExternalSandbox {
            network_access: NetworkAccess::Allowlist { services },
            ..
        }
        | SandboxPolicy::WorkspaceWrite {
            network_access: NetworkAccess::Allowlist { services },
            ..
        } => services,
        _ => return Ok(None),
    };

    if services.is_empty() {
        return Err("NetworkAccess::Allowlist has no services configured".to_string());
    }

    for service in services {
        if let Err(error) = service.validate() {
            return Err(format!(
                "allowlist service '{}' is invalid: {}",
                service.host, error,
            ));
        }
    }

    Ok(Some(AllowlistProxy::new(services)))
}

/// Caller-facing decision returned by [`select_network_proxy`].
///
/// Encodes the three-way outcome of the `(NetworkAccess-style mode,
/// proxy-startup result, SandboxEnforcement)` decision tree from
/// `docs/development/commands/sandbox.md` §7.4 lines 340-343 without forcing
/// callers to import `SandboxTransformError` (the transform layer
/// converts `Reject` into `NetworkEnforcementFailed` at the boundary).
///
/// Variants:
/// - [`Proxy`](Self::Proxy): the runtime should route outbound
///   connections through the contained `&'a dyn NetworkProxy`.
/// - [`DegradeToDenied`](Self::DegradeToDenied): the requested
///   allowlist proxy is unavailable but the enforcement tier allows
///   degrading silently or with a warning — the runtime should fall
///   back to `NoopProxy` (deny all) and surface the reason as a
///   tracing warning. The `reason` field is the human-readable
///   degradation message.
/// - [`Reject`](Self::Reject): the proxy is unavailable AND the
///   enforcement tier forbids degrading. The runtime should emit
///   `SandboxTransformError::NetworkEnforcementFailed { reason }`.
#[derive(Debug)]
pub enum NetworkProxySelection<'a> {
    /// Use this proxy for outbound network requests in this transform.
    Proxy(&'a dyn NetworkProxy),
    /// Allowlist requested but unavailable; the enforcement tier
    /// permits silent or warned degradation to deny-all. `reason`
    /// is meant to be emitted as a `tracing::warn!` event.
    DegradeToDenied { reason: String },
    /// Allowlist requested but unavailable AND the enforcement tier
    /// forbids degrading. Caller maps this to
    /// `SandboxTransformError::NetworkEnforcementFailed { reason }`.
    Reject { reason: String },
}

/// What kind of network policy the caller is asking for.
///
/// Pre-positions the Phase 7 (`docs/development/commands/sandbox.md` §7.1)
/// `NetworkAccess::{Denied, Allowlist, Full}` migration without
/// touching the existing 2-state `NetworkAccess` enum. Once Phase 7
/// lands, this type collapses into the three-state `NetworkAccess`
/// itself (or stays as a thin wrapper) — either way the
/// `select_network_proxy` decision tree implementation here doesn't
/// change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkAccessMode {
    /// Deny all outbound except loopback. Routes through `NoopProxy`.
    Denied,
    /// Allow a fixed allowlist via a per-host proxy. Routes through
    /// the supplied `allowlist_proxy` when available, otherwise
    /// degrades per `SandboxEnforcement`.
    Allowlist,
    /// Allow unrestricted outbound. Routes through `LoopbackOnlyProxy`
    /// as a diagnostic placeholder. `Full` requires `DangerFullAccess`
    /// or explicit approval.
    Full,
}

/// `SandboxEnforcement` analogue local to the proxy module.
///
/// Duplicated so the proxy module can stay decoupled from
/// `sandbox::policy` for testing. The caller maps from
/// `policy::SandboxEnforcement` at the transform boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyEnforcement {
    /// Must use a real allowlist proxy in `Allowlist` mode; fail
    /// closed if the proxy is unavailable.
    Required,
    /// Prefer the allowlist proxy; on unavailability, degrade to
    /// deny-all with a visible warning.
    PreferStrict,
    /// Prefer the allowlist proxy; on unavailability, silently
    /// degrade to deny-all.
    BestEffort,
}

/// Resolve which [`NetworkProxy`] the sandbox runtime should use for
/// an outbound request based on the three inputs from
/// `docs/development/commands/sandbox.md` §7.4 lines 340-343:
///
/// 1. The requested mode (`Denied` / `Allowlist` / `Full`).
/// 2. Whether the per-allowlist proxy has started
///    (`allowlist_proxy = Some(_)` vs `None`).
/// 3. The enforcement tier (`Required` / `PreferStrict` /
///    `BestEffort`).
///
/// Output `NetworkProxySelection` variants map per the doc:
///
/// | mode      | allowlist_proxy | enforcement     | result                          |
/// |-----------|-----------------|-----------------|---------------------------------|
/// | Denied    | n/a             | n/a             | Proxy(NoopProxy)                |
/// | Allowlist | Some(p)         | n/a             | Proxy(p)                        |
/// | Allowlist | None            | Required        | Reject (NetworkEnforcementFailed)|
/// | Allowlist | None            | PreferStrict    | DegradeToDenied + visible warn  |
/// | Allowlist | None            | BestEffort      | DegradeToDenied (silent)        |
/// | Full      | n/a             | n/a             | Proxy(LoopbackOnlyProxy)        |
///
/// `Full` returns `LoopbackOnlyProxy` as a diagnostic placeholder because no
/// proxy is needed for full network access.
pub fn select_network_proxy<'a>(
    mode: NetworkAccessMode,
    allowlist_proxy: Option<&'a dyn NetworkProxy>,
    enforcement: ProxyEnforcement,
) -> NetworkProxySelection<'a> {
    static NOOP: NoopProxy = NoopProxy;
    static LOOPBACK_ONLY: LoopbackOnlyProxy = LoopbackOnlyProxy;

    match mode {
        NetworkAccessMode::Denied => NetworkProxySelection::Proxy(&NOOP),
        NetworkAccessMode::Allowlist => match allowlist_proxy {
            Some(proxy) => NetworkProxySelection::Proxy(proxy),
            None => match enforcement {
                ProxyEnforcement::Required => NetworkProxySelection::Reject {
                    reason: "NetworkAccess::Allowlist requested but the per-allowlist proxy is \
                             unavailable; SandboxEnforcement::Required forbids degrading to Denied"
                        .to_string(),
                },
                ProxyEnforcement::PreferStrict => NetworkProxySelection::DegradeToDenied {
                    reason: "NetworkAccess::Allowlist requested but proxy unavailable; \
                             degrading to Denied under SandboxEnforcement::PreferStrict — \
                             this event must be surfaced as a visible warning in sandbox status"
                        .to_string(),
                },
                ProxyEnforcement::BestEffort => NetworkProxySelection::DegradeToDenied {
                    reason: "NetworkAccess::Allowlist requested but proxy unavailable; \
                             silently degrading to Denied under SandboxEnforcement::BestEffort"
                        .to_string(),
                },
            },
        },
        NetworkAccessMode::Full => NetworkProxySelection::Proxy(&LOOPBACK_ONLY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(host: &str, port: u16) -> NetworkRequest {
        NetworkRequest {
            host: host.to_string(),
            port,
            protocol: NetworkProtocol::Tcp,
        }
    }

    /// `is_loopback_host` must accept the literal `"localhost"` (any
    /// casing), `127.0.0.1`, and `::1`. Everything else — including
    /// the lookalike `localhost.evil.com` — is rejected.
    #[test]
    fn is_loopback_host_accepts_localhost_and_ipv4_v6_loopback_addresses() {
        for accepted in ["localhost", "LOCALHOST", "127.0.0.1", "127.0.0.2", "::1"] {
            assert!(
                is_loopback_host(accepted),
                "expected '{accepted}' to be loopback",
            );
        }
        for rejected in [
            "example.com",
            "192.168.1.1",
            "10.0.0.1",
            "::2",
            "localhost.evil.com",
            "",
        ] {
            assert!(
                !is_loopback_host(rejected),
                "expected '{rejected}' to NOT be loopback",
            );
        }
    }

    /// The three concrete [`NetworkProxy`] backends expose stable
    /// `backend_name()` strings that `libra sandbox status` surfaces
    /// verbatim in its `proxy_backend` field, and which audit
    /// consumers correlate decisions against. Pin all three in one
    /// place so a rename can't slip through (the per-proxy behaviour
    /// tests assert their own name, but only this test guarantees the
    /// *set* of names stays in sync with the
    /// `docs/commands/sandbox.md` `proxy_backend` field table).
    ///
    /// `describe_network_access` also emits the synthetic value
    /// `"none"` when an allowlist proxy is requested but unavailable
    /// (degrade / reject branches) — that string is owned by the
    /// command layer, not a `NetworkProxy::backend_name()`, so it is
    /// pinned by the `describe_network_access_*` tests in
    /// `src/command/sandbox.rs` rather than here.
    #[test]
    fn proxy_backend_names_are_stable() {
        assert_eq!(NoopProxy.backend_name(), "noop");
        assert_eq!(LoopbackOnlyProxy.backend_name(), "loopback-only");
        assert_eq!(
            AllowlistProxy::new(&[NetworkService {
                host: "registry.npmjs.org".to_string(),
                ports: vec![443],
                protocol: None,
            }])
            .backend_name(),
            "allowlist",
        );
    }

    /// `NoopProxy::evaluate` must deny every request regardless of
    /// host / port / protocol. Pin the rejection so a future
    /// "smart" NoopProxy that secretly allows some hosts fails this
    /// test.
    #[test]
    fn noop_proxy_denies_every_request() {
        let proxy = NoopProxy;
        assert_eq!(proxy.backend_name(), "noop");
        for (host, port) in [
            ("example.com", 443),
            ("127.0.0.1", 22),
            ("localhost", 80),
            ("registry.npmjs.org", 443),
        ] {
            let req = request(host, port);
            let decision = proxy.evaluate(&req);
            assert!(decision.is_deny(), "noop proxy must deny {host}:{port}");
            assert!(!decision.is_allow());
        }
    }

    /// `LoopbackOnlyProxy::evaluate` must allow loopback destinations
    /// (`127.0.0.1`, `::1`, `localhost`) and deny everything else.
    /// The denial reason must include the rejected host so audit
    /// records can pinpoint the offending target.
    #[test]
    fn loopback_only_proxy_allows_loopback_and_denies_remote_hosts() {
        let proxy = LoopbackOnlyProxy;
        assert_eq!(proxy.backend_name(), "loopback-only");

        for host in ["localhost", "127.0.0.1", "::1"] {
            let decision = proxy.evaluate(&request(host, 8080));
            assert!(
                decision.is_allow(),
                "loopback-only proxy must allow {host} (got {decision:?})",
            );
        }

        for host in ["example.com", "192.168.1.1", "registry.npmjs.org"] {
            let decision = proxy.evaluate(&request(host, 443));
            match decision {
                NetworkDecision::Deny(ref reason) => {
                    assert!(
                        reason.contains(host),
                        "deny reason must name the rejected host; got {reason}",
                    );
                }
                NetworkDecision::Allow => {
                    panic!("loopback-only proxy must deny non-loopback {host}");
                }
            }
        }
    }

    #[test]
    fn allowlist_proxy_matches_exact_hosts_and_protocols() {
        let services = vec![
            NetworkService {
                host: "registry.npmjs.org".to_string(),
                ports: vec![443],
                protocol: None,
            },
            NetworkService {
                host: "*.pypi.org".to_string(),
                ports: vec![443],
                protocol: Some(NetworkProtocol::Tcp),
            },
            NetworkService {
                host: "github.com".to_string(),
                ports: vec![53],
                protocol: Some(NetworkProtocol::Udp),
            },
        ];

        let proxy = AllowlistProxy::new(&services);

        for request in [
            request("registry.npmjs.org", 443),
            request("api.pypi.org", 443),
            request("foo.pypi.org", 443),
        ] {
            assert!(
                proxy.evaluate(&request).is_allow(),
                "allowed host/port mismatch: {request:?}"
            );
        }

        for request in [
            request("registry.npmjs.org", 8443),
            request("pypi.org", 443),
            request("github.com", 443),
            request("sub.github.com", 53),
            request("sub.pypi.org", 8443),
        ] {
            assert!(
                proxy.evaluate(&request).is_deny(),
                "request should be denied: {request:?}",
            );
        }

        let udp_mismatch = proxy.evaluate(&NetworkRequest {
            host: "github.com".to_string(),
            port: 53,
            protocol: NetworkProtocol::Tcp,
        });
        assert!(udp_mismatch.is_deny(), "protocol mismatch should deny");
    }

    #[test]
    fn allowlist_proxy_from_policy_handles_allowlist_and_ignores_invalid_shapes() {
        let services = vec![NetworkService {
            host: "registry.npmjs.org".to_string(),
            ports: vec![443],
            protocol: None,
        }];

        let allowlist_policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            network_access: NetworkAccess::Allowlist {
                services: services.clone(),
            },
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
            allow_metadata_writes: false,
        };
        assert!(
            allowlist_proxy_from_policy(&allowlist_policy)
                .expect("allowlist service list should be usable")
                .is_some()
        );

        let denied_policy = SandboxPolicy::ReadOnly;
        assert!(
            allowlist_proxy_from_policy(&denied_policy)
                .expect("non-allowlist policy should not produce a proxy")
                .is_none()
        );

        let invalid_policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            network_access: NetworkAccess::Allowlist {
                services: vec![NetworkService {
                    host: String::new(),
                    ports: vec![443],
                    protocol: None,
                }],
            },
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
            allow_metadata_writes: false,
        };
        let error = allowlist_proxy_from_policy(&invalid_policy)
            .expect_err("invalid host should be rejected by service validation");
        assert!(error.contains("allowlist service '' is invalid"));
    }

    /// The proxy trait must be object-safe so the sandbox runtime can
    /// borrow implementations through a dyn reference. This test
    /// exercises the dyn-safe path explicitly so a future trait change
    /// that adds a non-object-safe method (e.g. a generic) fails to
    /// compile here rather than at the sandbox-runtime callsite.
    /// explicitly so a future trait change that adds a non-object-safe
    /// method (e.g. a generic) fails to compile here rather than at
    /// the sandbox-runtime callsite.
    #[test]
    fn network_proxy_is_object_safe_and_dispatches_through_dyn_reference() {
        let proxies: &[&'static dyn NetworkProxy] = &[&NoopProxy, &LoopbackOnlyProxy];
        let req = request("localhost", 8080);
        let mut allowed = 0;
        let mut denied = 0;
        for proxy in proxies {
            match proxy.evaluate(&req) {
                NetworkDecision::Allow => allowed += 1,
                NetworkDecision::Deny(_) => denied += 1,
            }
        }
        // Exactly the loopback-only proxy should accept the loopback
        // request; the noop proxy denies it. The two-element split
        // proves the dyn dispatch hit both implementations.
        assert_eq!(allowed, 1);
        assert_eq!(denied, 1);
    }

    /// `select_network_proxy(Denied, _, _)` must always route to a
    /// proxy that denies everything, regardless of the supplied
    /// allowlist proxy or enforcement tier. Pin the denial across
    /// the entire enforcement / proxy-availability matrix so a
    /// future refactor that accidentally lets a `Some(proxy)` leak
    /// into the `Denied` arm fails this test.
    #[test]
    fn select_network_proxy_denied_mode_always_returns_a_denying_proxy() {
        let live: &'static dyn NetworkProxy = &LoopbackOnlyProxy;
        for enforcement in [
            ProxyEnforcement::Required,
            ProxyEnforcement::PreferStrict,
            ProxyEnforcement::BestEffort,
        ] {
            for proxy in [None, Some(live)] {
                let selection = select_network_proxy(NetworkAccessMode::Denied, proxy, enforcement);
                let NetworkProxySelection::Proxy(p) = selection else {
                    panic!(
                        "Denied mode must return Proxy(_); got {selection:?} \
                         (enforcement={enforcement:?}, proxy={})",
                        proxy.is_some()
                    );
                };
                // The returned proxy must deny a real-world host.
                let decision = p.evaluate(&request("example.com", 443));
                assert!(decision.is_deny(), "Denied mode proxy must deny outbound");
            }
        }
    }

    /// `select_network_proxy(Allowlist, Some(p), _)` must route to
    /// the supplied proxy regardless of enforcement tier — when the
    /// proxy is available, the enforcement tier never matters.
    #[test]
    fn select_network_proxy_allowlist_with_proxy_routes_to_supplied_backend() {
        let live: &'static dyn NetworkProxy = &LoopbackOnlyProxy;
        let live_name = live.backend_name();
        for enforcement in [
            ProxyEnforcement::Required,
            ProxyEnforcement::PreferStrict,
            ProxyEnforcement::BestEffort,
        ] {
            let selection =
                select_network_proxy(NetworkAccessMode::Allowlist, Some(live), enforcement);
            let NetworkProxySelection::Proxy(p) = selection else {
                panic!("Allowlist with proxy must return Proxy(_); enforcement={enforcement:?}");
            };
            assert_eq!(
                p.backend_name(),
                live_name,
                "must route to the supplied proxy",
            );
        }
    }

    /// `select_network_proxy(Allowlist, None, Required)` must
    /// return `Reject` — the caller maps this to
    /// `SandboxTransformError::NetworkEnforcementFailed`.
    #[test]
    fn select_network_proxy_allowlist_without_proxy_under_required_returns_reject() {
        let selection = select_network_proxy(
            NetworkAccessMode::Allowlist,
            None,
            ProxyEnforcement::Required,
        );
        match selection {
            NetworkProxySelection::Reject { reason } => {
                assert!(
                    reason.contains("Required"),
                    "reject reason must mention Required enforcement, got: {reason}",
                );
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    /// `select_network_proxy(Allowlist, None, PreferStrict | BestEffort)`
    /// must return `DegradeToDenied`. The reason text must mention
    /// the enforcement tier so audit consumers can tell whether the
    /// degradation was visible-warning (PreferStrict) or silent
    /// (BestEffort).
    #[test]
    fn select_network_proxy_allowlist_without_proxy_under_soft_enforcement_degrades() {
        for (enforcement, expected_text) in [
            (ProxyEnforcement::PreferStrict, "PreferStrict"),
            (ProxyEnforcement::BestEffort, "BestEffort"),
        ] {
            let selection = select_network_proxy(NetworkAccessMode::Allowlist, None, enforcement);
            match selection {
                NetworkProxySelection::DegradeToDenied { reason } => {
                    assert!(
                        reason.contains(expected_text),
                        "degrade reason must mention {expected_text}; got: {reason}",
                    );
                }
                other => panic!("expected DegradeToDenied for {enforcement:?}, got {other:?}"),
            }
        }
    }

    /// `Full` mode currently routes to `LoopbackOnlyProxy`.
    /// Pin the assertion so when the full pass-through proxy lands,
    /// this test fails and forces the implementer to update the
    /// expected backend name — that's exactly the kind of arm change
    /// audit consumers need to be aware of.
    #[test]
    fn select_network_proxy_full_mode_routes_to_loopback_only_placeholder() {
        let selection =
            select_network_proxy(NetworkAccessMode::Full, None, ProxyEnforcement::BestEffort);
        let NetworkProxySelection::Proxy(p) = selection else {
            panic!("Full mode must return Proxy(_); got {selection:?}");
        };
        assert_eq!(p.backend_name(), "loopback-only");
    }

    /// End-to-end integration test that exercises the full
    /// Phase 7 allowlist decision chain:
    ///
    ///   `policy::NetworkService` allowlist entries →
    ///   `NetworkRequest` (built from those entries) →
    ///   `NetworkProxy::evaluate` (via `select_network_proxy` dispatch)
    ///
    /// Validates that the validated allowlist entry shape (with
    /// explicit ports and protocol) can be threaded through the
    /// proxy dispatch surface without translation, and that the
    /// `AllowlistProxy` accepts a configured target and rejects a remote target
    /// with the same port.
    #[test]
    fn phase7_allowlist_proxy_chains_validated_allowlist_entry_to_proxy_decision() {
        use crate::internal::ai::sandbox::policy::{NetworkProtocol, NetworkService};

        // Build a well-formed allowlist entry — mimics a row from
        // `.libra/sandbox.toml [[sandbox.network.services]]`. The
        // entry passes validation (non-empty host, explicit ports).
        let entry = NetworkService {
            host: "localhost".to_string(),
            ports: vec![8080],
            protocol: Some(NetworkProtocol::Tcp),
        };
        entry
            .validate()
            .expect("well-formed allowlist entry must validate");

        // Translate to a NetworkRequest for each declared port through
        // the concrete allowlist proxy and verify that matching and
        // non-matching hosts are distinguished.
        let live: AllowlistProxy = AllowlistProxy::new(std::slice::from_ref(&entry));
        let selection = select_network_proxy(
            NetworkAccessMode::Allowlist,
            Some(&live),
            ProxyEnforcement::Required,
        );
        let NetworkProxySelection::Proxy(proxy) = selection else {
            panic!("Allowlist with proxy must return Proxy(_)");
        };

        for port in &entry.ports {
            let req = NetworkRequest {
                host: entry.host.clone(),
                port: *port,
                protocol: entry.effective_protocol(),
            };
            assert!(
                proxy.evaluate(&req).is_allow(),
                "allowlist proxy must allow target on declared port {port}",
            );
        }

        // A non-loopback host with the same allowlist port must be
        // rejected — proves the proxy is actually matching host
        // entries rather than accepting everything.
        let remote = NetworkRequest {
            host: "example.com".to_string(),
            port: entry.ports[0],
            protocol: entry.effective_protocol(),
        };
        let decision = proxy.evaluate(&remote);
        assert!(
            decision.is_deny(),
            "allowlist proxy must reject remote host even on allowlist port",
        );
    }
}
