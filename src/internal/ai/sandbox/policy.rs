//! Policy model for constraining AI tool execution inside a workspace sandbox.
//!
//! Boundary: policy parsing is conservative and treats missing or ambiguous allowlists
//! as denied operations. Hardening contract tests cover path traversal, shell command,
//! and workspace-scope boundaries.

use std::{
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};

/// Controls whether command execution uses the configured sandbox policy
/// or bypasses it for an escalated run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxPermissions {
    #[default]
    UseDefault,
    RequireEscalated,
}

impl SandboxPermissions {
    pub fn requires_escalated_permissions(self) -> bool {
        matches!(self, Self::RequireEscalated)
    }
}

/// Controls how strongly Libra requires an OS sandbox backend to be active.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxEnforcement {
    Required,
    PreferStrict,
    #[default]
    BestEffort,
}

impl SandboxEnforcement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::PreferStrict => "prefer_strict",
            Self::BestEffort => "best_effort",
        }
    }

    pub fn requires_effective_sandbox(self) -> bool {
        matches!(self, Self::Required)
    }

    /// Every variant of [`SandboxEnforcement`] in declaration order
    /// (`Required`, `PreferStrict`, `BestEffort`). The fixed-length
    /// array makes the enumeration size part of the public API — a
    /// future fourth tier requires extending this list in the same
    /// patch, which forces the [`as_str`](Self::as_str) match arms,
    /// the [`FromStr`](std::str::FromStr) parser, and the
    /// [`SandboxEnforcementParseError`] expected-list error message
    /// to all be revisited.
    pub fn all() -> [Self; 3] {
        [Self::Required, Self::PreferStrict, Self::BestEffort]
    }
}

impl std::str::FromStr for SandboxEnforcement {
    type Err = SandboxEnforcementParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "required" => Ok(Self::Required),
            "prefer_strict" | "prefer-strict" => Ok(Self::PreferStrict),
            "best_effort" | "best-effort" => Ok(Self::BestEffort),
            _ => Err(SandboxEnforcementParseError {
                value: value.to_string(),
            }),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error(
    "invalid sandbox enforcement '{value}'; expected one of: required, prefer_strict, best_effort"
)]
pub struct SandboxEnforcementParseError {
    value: String,
}

/// Wire-protocol selector for a [`NetworkService`] allowlist entry.
///
/// Pre-positioned for Phase 7 (`docs/development/commands/sandbox.md` §7.1) of the
/// sandbox network-three-state work. Until the full
/// `NetworkAccess::Allowlist { services }` migration lands this type is
/// used by the [`NetworkService`] schema, validators, and allowlist
/// proxy runtime. Keeping it explicit lets `.libra/sandbox.toml`
/// service entries map directly into proxy decisions.
///
/// `Tcp` is the default to match the sandbox.md spec
/// ("默认 tcp"); callers that need UDP-only allowlists (e.g. DNS, QUIC)
/// set `protocol = Some(NetworkProtocol::Udp)` on the service.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkProtocol {
    /// TCP — the default for `https://`, `git://`, `ssh://` services.
    #[default]
    Tcp,
    /// UDP — used by DNS, QUIC, and proprietary peer-to-peer
    /// transports.
    Udp,
}

/// One entry in a sandbox network allowlist.
///
/// Pre-positioned for Phase 7 (`docs/development/commands/sandbox.md` §7.1).
/// The shape matches the `.libra/sandbox.toml` `[[sandbox.network.services]]`
/// section:
///
/// ```toml
/// [[sandbox.network.services]]
/// host = "registry.npmjs.org"
/// ports = [443]
/// ```
///
/// Field semantics (mirrors sandbox.md §7.1):
/// - `host`: hostname or `*.subdomain` wildcard. Bare `"*"` (catch-all)
///   and the empty string are rejected by [`Self::validate`] because
///   they would silently turn an allowlist into a full-network grant.
/// - `ports`: empty = "every port allowed by the proxy". A non-empty
///   list restricts to the supplied ports. High-sensitivity ports
///   (22 / SSH, 3389 / RDP) are rejected by `validate` unless the
///   caller listed them explicitly — this catches a config that omits
///   `ports` for an entry whose hostname matches an SSH bastion, etc.
/// - `protocol`: `None` means "Tcp (the default)"; callers needing UDP
///   set `Some(NetworkProtocol::Udp)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NetworkService {
    /// Hostname or `*.subdomain` wildcard; never bare `"*"` or empty.
    pub host: String,
    /// Allowed destination ports. Empty = any port on the host.
    #[serde(default)]
    pub ports: Vec<u16>,
    /// Wire protocol; `None` = TCP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<NetworkProtocol>,
}

/// Validation error produced by [`NetworkService::validate`].
///
/// Each variant carries enough context to let
/// `.libra/sandbox.toml` parsers surface an actionable error to the
/// user without re-formatting the failure shape.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NetworkServiceValidationError {
    /// `host` was the empty string. The allowlist parser must reject
    /// these because an empty host trivially matches nothing under
    /// the proxy and would otherwise be silently dropped.
    #[error("network service host must not be empty")]
    EmptyHost,
    /// `host` was the bare wildcard `"*"`. Treated as a config error
    /// because it turns an allowlist into a catch-all grant — the
    /// user almost certainly meant `NetworkAccess::Full`.
    #[error(
        "network service host must not be the bare wildcard '*'; use NetworkAccess::Full for a catch-all grant"
    )]
    BareWildcardHost,
    /// `ports` was empty but `host` matched a high-sensitivity port
    /// pattern (22 / SSH, 3389 / RDP). The validator demands those
    /// ports be listed explicitly so the user can't open SSH access
    /// by accidentally writing `{ host = "bastion.example.com" }`
    /// without `ports`.
    #[error(
        "network service '{host}' allows high-sensitivity port {port} via empty ports list; \
         list the ports explicitly to opt in"
    )]
    HighSensitivityPortRequiresExplicitList { host: String, port: u16 },
}

/// High-sensitivity ports that must NEVER be granted via an empty
/// `ports` list. Port 22 = SSH; port 3389 = RDP. The sandbox.md
/// spec at §7.1 line 336 mandates these be listed explicitly.
const HIGH_SENSITIVITY_PORTS: &[u16] = &[22, 3389];

impl NetworkService {
    /// Validate this service entry against the rules in
    /// `docs/development/commands/sandbox.md` §7.1:
    ///
    /// - `host` must not be empty.
    /// - `host` must not be the bare wildcard `"*"`.
    /// - If `ports` is empty, the entry implicitly allows every port
    ///   — including the high-sensitivity SSH (22) / RDP (3389)
    ///   ports. The validator rejects the empty-ports form so the
    ///   user has to opt in explicitly.
    ///
    /// Returns `Ok(())` for a well-formed entry, or the matching
    /// [`NetworkServiceValidationError`] variant otherwise.
    pub fn validate(&self) -> Result<(), NetworkServiceValidationError> {
        if self.host.is_empty() {
            return Err(NetworkServiceValidationError::EmptyHost);
        }
        if self.host == "*" {
            return Err(NetworkServiceValidationError::BareWildcardHost);
        }
        if self.ports.is_empty()
            && let Some(&port) = HIGH_SENSITIVITY_PORTS.first()
        {
            // Empty `ports` means "any port", which silently includes
            // the high-sensitivity ports tracked in
            // [`HIGH_SENSITIVITY_PORTS`]. Force the caller to list
            // ports explicitly so an entry that omits `ports` for
            // (say) a hostname that resolves to an SSH bastion
            // can't open port 22 by accident. The error surfaces the
            // first sensitive port from the canonical list — that's
            // enough to point the user at the rule, and listing the
            // ports explicitly satisfies the validator regardless of
            // which sensitive port the host actually exposes.
            return Err(
                NetworkServiceValidationError::HighSensitivityPortRequiresExplicitList {
                    host: self.host.clone(),
                    port,
                },
            );
        }
        Ok(())
    }

    /// Effective protocol — `Tcp` when `protocol` is `None`. Avoids
    /// callers having to `unwrap_or(Tcp)` at every dispatch site once
    /// Phase 7.4's proxy starts routing per-service.
    pub fn effective_protocol(&self) -> NetworkProtocol {
        self.protocol.unwrap_or_default()
    }
}

/// Sandbox network access mode. Per `docs/development/commands/sandbox.md`
/// §7.1 the runtime supports a three-state contract:
///
/// - [`NetworkAccess::Denied`] (the default): no outbound network
///   permitted; the sandbox sets `LIBRA_SANDBOX_NETWORK_DISABLED` and
///   the proxy backend selects the noop loopback-only proxy.
/// - [`NetworkAccess::Allowlist`]: outbound network permitted only
///   to the listed [`NetworkService`] entries (host + port + protocol).
///   The proxy backend routes matching traffic through the allowlist
///   proxy and drops everything else.
/// - [`NetworkAccess::Full`]: unconstrained outbound network. Used
///   only by explicit-escalation policies (`DangerFullAccess`) or
///   when a user toggles `--network` for the legacy `WorkspaceWrite`
///   shape.
///
/// The 2-state predecessor (`Restricted` / `Enabled`) is gone. The
/// `is_enabled` helper now returns `true` for both `Allowlist` and
/// `Full` so existing "is the network available at all" gates keep
/// working; new sites that need to distinguish the three modes
/// match on the enum directly.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum NetworkAccess {
    /// No outbound network. Loopback only via the noop proxy.
    #[default]
    Denied,
    /// Outbound network permitted only for the listed services. Per
    /// sandbox.md §7.1 the services must each pass
    /// [`NetworkService::validate`] (no empty / bare-wildcard host,
    /// no implicit high-sensitivity-port grants).
    Allowlist {
        #[serde(default)]
        services: Vec<NetworkService>,
    },
    /// Unconstrained outbound network — used by
    /// [`SandboxPolicy::DangerFullAccess`] and any explicit-escalation
    /// path. The sandbox does NOT set
    /// `LIBRA_SANDBOX_NETWORK_DISABLED`.
    Full,
}

/// Custom `Deserialize` for [`NetworkAccess`] that accepts both the
/// current tagged form (`{"mode": "denied" | "allowlist" | "full",
/// ...}`) and the legacy boolean form
/// (`true` → [`NetworkAccess::Full`], `false` → [`NetworkAccess::Denied`])
/// that older `.libra/sandbox.toml` files and persisted
/// `SandboxPolicy::WorkspaceWrite` JSON envelopes used before the Phase 7
/// three-state migration (see `docs/development/commands/sandbox.md` §7.1).
///
/// The legacy form keeps configs and Codex-style JSON envelopes written
/// against the previous 2-state [`NetworkAccess`] readable after the
/// upgrade; new writers always emit the tagged form via the derived
/// `Serialize`.
///
/// Uses a custom [`serde::de::Visitor`] (rather than the simpler
/// `#[serde(untagged)]` enum trick) so that off-contract inputs
/// (`null`, numbers, bare strings like `"full"`, arrays, ...) surface
/// an actionable error message — "expected either a legacy boolean
/// (true → Full, false → Denied) or a tagged object {mode: ...}" —
/// instead of the generic "data did not match any variant of untagged
/// enum" that the derived `Deserialize` produces. The map arm still
/// delegates to a derived `Deserialize` for the tagged shape so the
/// `kebab-case` `mode` discriminator and `services` payload remain
/// in lockstep with [`Serialize`].
impl<'de> Deserialize<'de> for NetworkAccess {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "mode", rename_all = "kebab-case")]
        enum Tagged {
            Denied,
            Allowlist {
                #[serde(default)]
                services: Vec<NetworkService>,
            },
            Full,
        }

        struct NetworkAccessVisitor;

        impl<'de> serde::de::Visitor<'de> for NetworkAccessVisitor {
            type Value = NetworkAccess;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str(
                    "either a legacy boolean (true → Full, false → Denied) or a tagged object \
                     {\"mode\": \"denied\" | \"allowlist\" | \"full\", ...}",
                )
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(NetworkAccess::from_legacy_bool(value))
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let tagged =
                    Tagged::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(match tagged {
                    Tagged::Denied => NetworkAccess::Denied,
                    Tagged::Allowlist { services } => NetworkAccess::Allowlist { services },
                    Tagged::Full => NetworkAccess::Full,
                })
            }
        }

        deserializer.deserialize_any(NetworkAccessVisitor)
    }
}

impl NetworkAccess {
    /// `true` when outbound network is permitted at all — `Full` or
    /// `Allowlist`. The disable-network env-var gate and the
    /// `has_full_network_access` policy helper use this predicate to
    /// decide whether to set `LIBRA_SANDBOX_NETWORK_DISABLED`.
    ///
    /// `Allowlist` is intentionally treated as "enabled" here: the
    /// outbound proxy must be reachable for the listed services, so
    /// the disable-network env var would defeat the purpose. Sites
    /// that need to distinguish `Full` from `Allowlist` should call
    /// [`is_full`](Self::is_full) / [`is_allowlist`](Self::is_allowlist).
    pub fn is_enabled(&self) -> bool {
        !matches!(self, Self::Denied)
    }

    /// Positive predicate for `Denied`. Companion to
    /// [`is_enabled`](Self::is_enabled) so call sites that want to
    /// gate on "is the network locked down?" can express that intent
    /// directly. The `!is_enabled() == is_denied()` invariant is
    /// pinned by regression tests.
    pub fn is_denied(&self) -> bool {
        matches!(self, Self::Denied)
    }

    /// `true` for the unconstrained `Full` variant only. Used by
    /// callers that must distinguish "any network" (Full +
    /// Allowlist) from "absolutely no constraints".
    pub fn is_full(&self) -> bool {
        matches!(self, Self::Full)
    }

    /// `true` for the `Allowlist` variant. Carries no payload —
    /// callers needing the service list should match on the enum.
    pub fn is_allowlist(&self) -> bool {
        matches!(self, Self::Allowlist { .. })
    }

    /// Borrow the configured allowlist when the variant is
    /// `Allowlist`. Returns `None` for the other two variants so
    /// callers can simplify a `match` to a single-line helper.
    pub fn allowlist_services(&self) -> Option<&[NetworkService]> {
        match self {
            Self::Allowlist { services } => Some(services.as_slice()),
            _ => None,
        }
    }

    /// Construct a `Full`-equivalent from a legacy boolean toggle.
    /// `true` → `Full`, `false` → `Denied`. Used at the
    /// `.libra/sandbox.toml` parser boundary and by tests migrating
    /// from the previous 2-state shape.
    pub fn from_legacy_bool(allowed: bool) -> Self {
        if allowed { Self::Full } else { Self::Denied }
    }

    /// Restrictiveness rank: `Denied` (0) < `Allowlist` (1) < `Full`
    /// (2). Lower = more locked-down. Used by [`Self::restrict_with`]
    /// to pick the more-restrictive of two access settings.
    pub(crate) fn restrictiveness_rank(&self) -> u8 {
        match self {
            Self::Denied => 0,
            Self::Allowlist { .. } => 1,
            Self::Full => 2,
        }
    }

    /// Combine this policy-derived access with an operator-supplied
    /// `.libra/sandbox.toml [sandbox.network]` setting, returning the
    /// **more restrictive** of the two. This is a tightening-only
    /// operation: the result can never grant more network reach than
    /// either input, so a config file can lock a workspace down but
    /// never silently widen a policy.
    ///
    /// Per `docs/development/commands/sandbox.md` §7.5, *upgrading* network
    /// access (e.g. opening an allowlist from a `Denied` baseline)
    /// requires the `ExecApprovalRequest` channel; this combiner
    /// deliberately does not perform that upgrade. A config `Full`
    /// applied to a `Denied` policy therefore collapses back to
    /// `Denied` rather than loosening it.
    ///
    /// Semantics by rank:
    /// - Different ranks → the lower-ranked (more restrictive) value
    ///   wins, carrying its own services when it is the `Allowlist`.
    /// - Both `Allowlist` → the **intersection** of the two service
    ///   lists (by full [`NetworkService`] equality), which is a
    ///   subset of each input and therefore still strictly tightening.
    /// - Both `Denied` or both `Full` → that shared value.
    ///
    /// An `Allowlist` result that ends up with **no services** (an
    /// empty config allowlist, or an empty intersection) is collapsed
    /// to [`NetworkAccess::Denied`]. An allowlist that permits zero
    /// hosts is semantically deny-all, and collapsing it is also a
    /// safety requirement: the OS-layer transform builds bwrap /
    /// seatbelt args from the `Allowlist` *mode* (which shares the
    /// network namespace) and relies on the proxy to filter. With no
    /// services the proxy is never started, so an empty `Allowlist`
    /// reaching the transform under the `PreferStrict` / `BestEffort`
    /// tiers would leave the namespace shared with only a soft env-var
    /// signal. Returning `Denied` forces `--unshare-net` instead.
    pub fn restrict_with(&self, config: &NetworkAccess) -> NetworkAccess {
        let combined = match self
            .restrictiveness_rank()
            .cmp(&config.restrictiveness_rank())
        {
            std::cmp::Ordering::Less => self.clone(),
            std::cmp::Ordering::Greater => config.clone(),
            std::cmp::Ordering::Equal => match (self, config) {
                (Self::Allowlist { services: lhs }, Self::Allowlist { services: rhs }) => {
                    let intersection = lhs
                        .iter()
                        .filter(|service| rhs.contains(service))
                        .cloned()
                        .collect();
                    Self::Allowlist {
                        services: intersection,
                    }
                }
                // Both `Denied` or both `Full` — identical, return either.
                _ => self.clone(),
            },
        };

        match combined {
            Self::Allowlist { services } if services.is_empty() => Self::Denied,
            other => other,
        }
    }
}

pub fn sensitive_read_paths(home: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(home) = home {
        for relative in [
            ".ssh",
            ".aws",
            ".gnupg",
            ".netrc",
            ".azure",
            ".docker",
            ".npmrc",
            ".pypirc",
            ".cargo/credentials",
            ".cargo/credentials.toml",
            ".gem/credentials",
            ".config/gcloud",
            ".config/gh",
            ".config/hub",
            ".kube",
            ".config/libra/vault",
            ".mozilla/firefox",
            ".config/google-chrome",
            ".config/chromium",
            ".config/BraveSoftware/Brave-Browser",
            ".var/app/org.mozilla.firefox",
            "Library/Application Support/Google/Chrome",
            "Library/Application Support/Chromium",
            "Library/Application Support/BraveSoftware/Brave-Browser",
            "Library/Application Support/Firefox",
            "Library/Cookies",
        ] {
            paths.push(home.join(relative));
        }
    }

    paths.push(PathBuf::from("/etc/shadow"));
    paths
}

/// Runtime sandbox policy for shell-like tools.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SandboxPolicy {
    DangerFullAccess,
    ReadOnly,
    ExternalSandbox {
        #[serde(default)]
        network_access: NetworkAccess,
    },
    WorkspaceWrite {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        writable_roots: Vec<PathBuf>,
        /// Three-state network policy per `docs/development/commands/sandbox.md`
        /// §7.1: `Denied` / `Allowlist { services }` / `Full`. The
        /// legacy `bool` (which mapped `true → Full`, `false →
        /// Denied`) was migrated in v0.17.723; callers needing the
        /// boolean form should use `network_access.is_enabled()`.
        #[serde(default)]
        network_access: NetworkAccess,
        #[serde(default)]
        exclude_tmpdir_env_var: bool,
        #[serde(default)]
        exclude_slash_tmp: bool,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SandboxPolicyError {
    #[error(
        "refusing writable_root '{root}' because {reason}; choose a non-privileged project directory, expose the tool through a narrow proxy, or rerun with explicit escalated permissions if host-level access is intentional"
    )]
    DangerousWritableRoot { root: PathBuf, reason: &'static str },
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self::WorkspaceWrite {
            writable_roots: vec![],
            network_access: NetworkAccess::Denied,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WritableRoot {
    pub root: PathBuf,
    pub read_only_subpaths: Vec<PathBuf>,
}

impl WritableRoot {
    pub fn is_path_writable(&self, path: &Path) -> bool {
        if !path.starts_with(&self.root) {
            return false;
        }
        !self
            .read_only_subpaths
            .iter()
            .any(|subpath| path.starts_with(subpath))
    }
}

impl SandboxPolicy {
    pub fn new_read_only_policy() -> Self {
        Self::ReadOnly
    }

    pub fn new_workspace_write_policy() -> Self {
        Self::default()
    }

    /// Return a copy of this policy with its writable roots rebased onto
    /// a single sub-agent workspace root (CEX-S2-12 / S2-INV-03).
    ///
    /// When a sub-agent runs in a materialized isolated workspace, the
    /// inherited [`Self::WorkspaceWrite`] policy still carries the
    /// *parent's* `writable_roots` (the main worktree), which would let
    /// an absolute-path `shell` write escape the workspace. Rebasing the
    /// writable roots to `[workspace_root]` is what makes the OS sandbox
    /// deny those escapes; re-rooting the tool registry's working dir
    /// alone only redirects *relative* paths.
    ///
    /// `ReadOnly` is already non-writable, so it is returned unchanged.
    /// `DangerFullAccess` / `ExternalSandbox` deliberately opt out of
    /// `writable_roots` enforcement (full-disk or externally-managed),
    /// so they cannot be tightened here — they are returned unchanged
    /// with a warning that workspace isolation is best-effort under
    /// those postures (silently upgrading them would override the
    /// operator's explicit choice).
    pub fn rebased_to_workspace(&self, workspace_root: &Path) -> Self {
        match self {
            Self::WorkspaceWrite {
                network_access,
                exclude_tmpdir_env_var,
                exclude_slash_tmp,
                ..
            } => Self::WorkspaceWrite {
                writable_roots: vec![workspace_root.to_path_buf()],
                network_access: network_access.clone(),
                exclude_tmpdir_env_var: *exclude_tmpdir_env_var,
                exclude_slash_tmp: *exclude_slash_tmp,
            },
            Self::ReadOnly => Self::ReadOnly,
            Self::DangerFullAccess | Self::ExternalSandbox { .. } => {
                tracing::warn!(
                    workspace_root = %workspace_root.display(),
                    policy = ?self,
                    "sub-agent workspace isolation is best-effort under a full-disk / \
                     external-sandbox policy: writable_roots cannot be narrowed to the \
                     workspace, so absolute-path writes are not OS-denied",
                );
                self.clone()
            }
        }
    }

    pub fn has_full_disk_write_access(&self) -> bool {
        matches!(self, Self::DangerFullAccess | Self::ExternalSandbox { .. })
    }

    /// `true` when the policy permits ANY outbound network — either
    /// the unconstrained `Full` mode or the proxy-mediated
    /// `Allowlist`. Callers gating "should `LIBRA_SANDBOX_NETWORK_DISABLED`
    /// be set?" use this predicate; callers needing the strict
    /// "unconstrained network" semantic should match the underlying
    /// [`NetworkAccess`] for `Full` directly.
    pub fn has_full_network_access(&self) -> bool {
        match self {
            Self::DangerFullAccess => true,
            Self::ReadOnly => false,
            Self::ExternalSandbox { network_access } => network_access.is_enabled(),
            Self::WorkspaceWrite { network_access, .. } => network_access.is_enabled(),
        }
    }

    /// Return a copy of this policy with its network access tightened
    /// by an operator-supplied `.libra/sandbox.toml [sandbox.network]`
    /// setting (see [`NetworkAccess::restrict_with`]). The restriction
    /// is tightening-only: it can lock the workspace down but never
    /// widen the policy's reach.
    ///
    /// Only the network-bearing variants ([`Self::WorkspaceWrite`] and
    /// [`Self::ExternalSandbox`]) are affected. [`Self::ReadOnly`] is
    /// already `Denied`, and [`Self::DangerFullAccess`] is an explicit
    /// host-level escalation with no `network_access` field — the
    /// config file does not silently downgrade it (an operator who set
    /// `DangerFullAccess` opted out of the sandbox entirely), so it is
    /// returned unchanged.
    pub fn with_network_restriction(&self, config: &NetworkAccess) -> SandboxPolicy {
        match self {
            Self::ExternalSandbox { network_access } => Self::ExternalSandbox {
                network_access: network_access.restrict_with(config),
            },
            Self::WorkspaceWrite {
                writable_roots,
                network_access,
                exclude_tmpdir_env_var,
                exclude_slash_tmp,
            } => Self::WorkspaceWrite {
                writable_roots: writable_roots.clone(),
                network_access: network_access.restrict_with(config),
                exclude_tmpdir_env_var: *exclude_tmpdir_env_var,
                exclude_slash_tmp: *exclude_slash_tmp,
            },
            Self::DangerFullAccess | Self::ReadOnly => self.clone(),
        }
    }

    /// Return a copy of this policy with `network_access` explicitly
    /// replaced for network-bearing variants.
    ///
    /// This helper is used when runtime callers intentionally want to
    /// apply a specific network mode (for example, after explicit
    /// user approval) before applying any `.libra/sandbox.toml`
    /// tightening rules.
    pub fn with_network_access(&self, network_access: &NetworkAccess) -> Self {
        match self {
            Self::ExternalSandbox { .. } => Self::ExternalSandbox {
                network_access: network_access.clone(),
            },
            Self::WorkspaceWrite {
                writable_roots,
                exclude_tmpdir_env_var,
                exclude_slash_tmp,
                network_access: _,
            } => Self::WorkspaceWrite {
                writable_roots: writable_roots.clone(),
                network_access: network_access.clone(),
                exclude_tmpdir_env_var: *exclude_tmpdir_env_var,
                exclude_slash_tmp: *exclude_slash_tmp,
            },
            Self::DangerFullAccess | Self::ReadOnly => self.clone(),
        }
    }

    pub fn validate_writable_roots_with_cwd(&self, cwd: &Path) -> Result<(), SandboxPolicyError> {
        for root in self.writable_root_paths_with_cwd(cwd) {
            validate_writable_root(&root)?;
        }
        Ok(())
    }

    /// Returns writable roots resolved against the current working directory.
    /// Each writable root has protected subpaths (for example `.git`, `.libra`)
    /// that remain read-only.
    pub fn get_writable_roots_with_cwd(&self, cwd: &Path) -> Vec<WritableRoot> {
        self.writable_root_paths_with_cwd(cwd)
            .into_iter()
            .map(|root| WritableRoot {
                // Exact writable files are narrow exceptions beneath an
                // otherwise protected metadata directory. Appending
                // `file/.git`-style pseudo-children makes bubblewrap fail with
                // ENOTDIR and has no security value because a regular file has
                // no descendants to protect.
                read_only_subpaths: if root.is_file() {
                    Vec::new()
                } else {
                    // Missing roots are conservatively treated as directories:
                    // a command may create them after sandbox setup, and their
                    // future metadata children must remain protected.
                    protected_subpaths(&root)
                },
                root,
            })
            .collect()
    }

    fn writable_root_paths_with_cwd(&self, cwd: &Path) -> Vec<PathBuf> {
        match self {
            Self::DangerFullAccess | Self::ExternalSandbox { .. } | Self::ReadOnly => Vec::new(),
            Self::WorkspaceWrite {
                writable_roots,
                exclude_tmpdir_env_var,
                exclude_slash_tmp,
                network_access: _,
            } => {
                let mut roots: Vec<PathBuf> = Vec::new();

                for root in writable_roots {
                    push_root_unique(&mut roots, resolve_root(root, cwd));
                }

                if roots.is_empty() {
                    push_root_unique(&mut roots, cwd.to_path_buf());
                }

                if cfg!(unix) && !exclude_slash_tmp {
                    let slash_tmp = PathBuf::from("/tmp");
                    if slash_tmp.is_dir() {
                        push_root_unique(&mut roots, slash_tmp);
                    }
                }

                if !exclude_tmpdir_env_var && let Some(tmpdir) = std::env::var_os("TMPDIR") {
                    let tmpdir_path = PathBuf::from(tmpdir);
                    if tmpdir_path.is_absolute() && tmpdir_path.is_dir() {
                        push_root_unique(&mut roots, tmpdir_path);
                    }
                }

                roots
            }
        }
    }
}

fn resolve_root(root: &Path, cwd: &Path) -> PathBuf {
    if root.is_absolute() {
        root.to_path_buf()
    } else {
        cwd.join(root)
    }
}

fn push_root_unique(roots: &mut Vec<PathBuf>, root: PathBuf) {
    let normalized = root.canonicalize().unwrap_or(root);
    if roots.iter().any(|existing| existing == &normalized) {
        return;
    }
    roots.push(normalized);
}

fn validate_writable_root(root: &Path) -> Result<(), SandboxPolicyError> {
    let lexical = normalize_path_lexically(root);
    if let Some(reason) = dangerous_writable_root_reason(&lexical) {
        return Err(SandboxPolicyError::DangerousWritableRoot {
            root: lexical,
            reason,
        });
    }

    if let Ok(canonical) = root.canonicalize() {
        let canonical = normalize_path_lexically(&canonical);
        if let Some(reason) = dangerous_writable_root_reason(&canonical) {
            return Err(SandboxPolicyError::DangerousWritableRoot {
                root: canonical,
                reason,
            });
        }
    }

    Ok(())
}

fn normalize_path_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push("..");
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn dangerous_writable_root_reason(path: &Path) -> Option<&'static str> {
    if path == Path::new("/") {
        return Some("it would make the whole host filesystem writable from the sandbox");
    }
    for sensitive_root in ["/proc", "/sys", "/dev"] {
        if path == Path::new(sensitive_root) || path.starts_with(sensitive_root) {
            return Some("kernel and device files can be used to escape or weaken the sandbox");
        }
    }
    if path.file_name() == Some(OsStr::new("docker.sock")) {
        return Some("Docker socket access is equivalent to host-level container control");
    }
    if path.file_name() == Some(OsStr::new("containerd.sock")) {
        return Some("containerd socket access is equivalent to host-level container control");
    }
    if path == Path::new("/run/containerd/containerd.sock")
        || path == Path::new("/var/run/containerd/containerd.sock")
    {
        return Some("containerd socket access is equivalent to host-level container control");
    }
    if path.starts_with("/var/run/libvirt") || path.starts_with("/run/libvirt") {
        return Some("libvirt control sockets can start privileged host resources");
    }
    None
}

fn protected_subpaths(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for subdir in [".git", ".libra", ".codex", ".agents"] {
        paths.push(root.join(subdir));
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CEX-S2-12 / S2-INV-03: rebasing a `WorkspaceWrite` policy onto a
    /// sub-agent workspace must replace `writable_roots` with exactly
    /// the workspace root while preserving the network / tmpdir knobs,
    /// so the OS sandbox denies absolute-path writes to the parent
    /// worktree. The other three policy postures are returned unchanged.
    #[test]
    fn rebased_to_workspace_narrows_writable_roots_for_workspace_write() {
        let workspace = Path::new("/tmp/libra-task-workspace");

        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![PathBuf::from("/repo/main-worktree")],
            network_access: NetworkAccess::Full,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        };
        match policy.rebased_to_workspace(workspace) {
            SandboxPolicy::WorkspaceWrite {
                writable_roots,
                network_access,
                exclude_tmpdir_env_var,
                exclude_slash_tmp,
            } => {
                assert_eq!(writable_roots, vec![workspace.to_path_buf()]);
                assert_eq!(network_access, NetworkAccess::Full);
                assert!(exclude_tmpdir_env_var);
                assert!(exclude_slash_tmp);
            }
            other => panic!("expected WorkspaceWrite, got {other:?}"),
        }
    }

    #[test]
    fn rebased_to_workspace_leaves_non_workspace_write_policies_unchanged() {
        let workspace = Path::new("/tmp/libra-task-workspace");

        assert_eq!(
            SandboxPolicy::ReadOnly.rebased_to_workspace(workspace),
            SandboxPolicy::ReadOnly,
        );
        assert_eq!(
            SandboxPolicy::DangerFullAccess.rebased_to_workspace(workspace),
            SandboxPolicy::DangerFullAccess,
        );
        let external = SandboxPolicy::ExternalSandbox {
            network_access: NetworkAccess::Denied,
        };
        assert_eq!(external.rebased_to_workspace(workspace), external);
    }

    #[test]
    fn sandbox_enforcement_accepts_stable_spellings() {
        assert_eq!(
            "required".parse::<SandboxEnforcement>(),
            Ok(SandboxEnforcement::Required)
        );
        assert_eq!(
            "prefer-strict".parse::<SandboxEnforcement>(),
            Ok(SandboxEnforcement::PreferStrict)
        );
        assert_eq!(
            "best_effort".parse::<SandboxEnforcement>(),
            Ok(SandboxEnforcement::BestEffort)
        );
    }

    #[test]
    fn sandbox_enforcement_rejects_unknown_values() {
        let error = "strict"
            .parse::<SandboxEnforcement>()
            .expect_err("unsupported enforcement names must be rejected");

        assert_eq!(
            error.to_string(),
            "invalid sandbox enforcement 'strict'; expected one of: required, prefer_strict, best_effort"
        );
    }

    #[test]
    fn sensitive_read_paths_include_home_credentials_and_system_shadow() {
        let paths = sensitive_read_paths(Some(Path::new("/home/tester")));

        assert!(paths.contains(&PathBuf::from("/home/tester/.ssh")));
        assert!(paths.contains(&PathBuf::from("/home/tester/.aws")));
        assert!(paths.contains(&PathBuf::from("/home/tester/.netrc")));
        assert!(paths.contains(&PathBuf::from("/home/tester/.config/gh")));
        assert!(paths.contains(&PathBuf::from("/home/tester/.docker")));
        assert!(paths.contains(&PathBuf::from("/home/tester/.cargo/credentials.toml")));
        assert!(paths.contains(&PathBuf::from("/home/tester/.config/google-chrome")));
        assert!(paths.contains(&PathBuf::from("/home/tester/.mozilla/firefox")));
        assert!(paths.contains(&PathBuf::from("/home/tester/Library/Cookies")));
        assert!(paths.contains(&PathBuf::from("/etc/shadow")));
    }

    #[test]
    fn explicit_workspace_roots_do_not_expand_to_cwd() {
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![PathBuf::from("src/main.rs")],
            network_access: NetworkAccess::Denied,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        };

        let roots = policy.get_writable_roots_with_cwd(Path::new("/tmp/workspace"));

        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].root, PathBuf::from("/tmp/workspace/src/main.rs"));
        assert_eq!(roots[0].read_only_subpaths.len(), 4);
    }

    #[test]
    fn exact_writable_file_has_no_impossible_protected_children() {
        let temp = tempfile::tempdir().expect("create exact-file policy fixture");
        let file = temp.path().join("COMMIT_EDITMSG");
        std::fs::write(&file, b"message").expect("create exact writable file");
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![file.clone()],
            network_access: NetworkAccess::Denied,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        };

        let roots = policy.get_writable_roots_with_cwd(temp.path());

        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].root, file);
        assert!(roots[0].read_only_subpaths.is_empty());
    }

    #[test]
    fn empty_workspace_roots_fall_back_to_cwd() {
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            network_access: NetworkAccess::Denied,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        };

        let roots = policy.get_writable_roots_with_cwd(Path::new("/tmp/workspace"));

        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].root, PathBuf::from("/tmp/workspace"));
    }

    #[test]
    fn dangerous_socket_writable_roots_are_rejected() {
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![
                PathBuf::from("/var/run/docker.sock"),
                PathBuf::from("/tmp/project"),
            ],
            network_access: NetworkAccess::Denied,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        };

        let error = policy
            .validate_writable_roots_with_cwd(Path::new("/tmp/workspace"))
            .expect_err("docker socket writable roots must be rejected");

        assert!(error.to_string().contains("Docker socket access"));
    }

    #[test]
    fn nested_docker_socket_roots_are_rejected() {
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![PathBuf::from("tools/docker.sock")],
            network_access: NetworkAccess::Denied,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        };

        let error = policy
            .validate_writable_roots_with_cwd(Path::new("/tmp/workspace"))
            .expect_err("glob-style docker.sock writable roots must be rejected");

        assert!(error.to_string().contains("Docker socket access"));
    }

    #[test]
    fn kernel_and_device_writable_roots_are_rejected() {
        for root in ["/", "/proc", "/proc/self", "/sys", "/dev", "/dev/null"] {
            let policy = SandboxPolicy::WorkspaceWrite {
                writable_roots: vec![PathBuf::from(root)],
                network_access: NetworkAccess::Denied,
                exclude_tmpdir_env_var: true,
                exclude_slash_tmp: true,
            };

            assert!(
                policy
                    .validate_writable_roots_with_cwd(Path::new("/tmp/workspace"))
                    .is_err(),
                "{root} must not be accepted as a writable sandbox root",
            );
        }
    }

    #[test]
    fn safe_workspace_writable_roots_are_accepted() {
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![PathBuf::from("src")],
            network_access: NetworkAccess::Denied,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        };

        policy
            .validate_writable_roots_with_cwd(Path::new("/tmp/workspace"))
            .expect("ordinary workspace roots should be accepted");
    }

    #[test]
    fn sandbox_policy_error_display_pins_dangerous_writable_root_template() {
        let err = SandboxPolicyError::DangerousWritableRoot {
            root: PathBuf::from("/etc"),
            reason: "is a system configuration directory",
        };
        assert_eq!(
            err.to_string(),
            "refusing writable_root '/etc' because is a system configuration directory; \
             choose a non-privileged project directory, expose the tool through a narrow \
             proxy, or rerun with explicit escalated permissions if host-level access is \
             intentional",
        );
    }

    /// `SandboxEnforcement::all()` enumerates every variant in
    /// declaration order, cross-checks each variant's `as_str()`
    /// against an exhaustive match (so a future fourth tier fails to
    /// compile here unless `all()` is also extended), and round-trips
    /// every canonical string through `FromStr`. Mirrors the
    /// v0.17.660+ `*::all()` + round-trip pattern.
    #[test]
    fn sandbox_enforcement_all_enumerates_every_variant_and_round_trips() {
        let variants = SandboxEnforcement::all();
        assert_eq!(variants.len(), 3);
        assert_eq!(
            variants,
            [
                SandboxEnforcement::Required,
                SandboxEnforcement::PreferStrict,
                SandboxEnforcement::BestEffort,
            ]
        );

        for variant in SandboxEnforcement::all() {
            let canonical = variant.as_str();
            let expected_canonical = match variant {
                SandboxEnforcement::Required => "required",
                SandboxEnforcement::PreferStrict => "prefer_strict",
                SandboxEnforcement::BestEffort => "best_effort",
            };
            assert_eq!(canonical, expected_canonical);

            let parsed: SandboxEnforcement = canonical
                .parse()
                .expect("canonical as_str() must round-trip through FromStr");
            assert_eq!(parsed, variant);
        }
    }

    /// `NetworkProtocol` must round-trip through serde as kebab-case
    /// (`"tcp"` / `"udp"`), default to `Tcp`, and `Hash` + `Eq` must
    /// hold so callers can index allowlists by protocol.
    #[test]
    fn network_protocol_serde_round_trip_and_defaults_to_tcp() {
        assert_eq!(NetworkProtocol::default(), NetworkProtocol::Tcp);
        for (variant, expected) in [
            (NetworkProtocol::Tcp, "\"tcp\""),
            (NetworkProtocol::Udp, "\"udp\""),
        ] {
            let serialised = serde_json::to_string(&variant).unwrap();
            assert_eq!(serialised, expected, "round-trip for {variant:?}");
            let back: NetworkProtocol = serde_json::from_str(&serialised).unwrap();
            assert_eq!(back, variant);
        }
    }

    /// `NetworkService::validate()` must reject the empty-host and
    /// bare-wildcard host shapes — both turn an allowlist into a
    /// silent grant. Pin the error variants explicitly so a future
    /// permissiveness in the validator fails the test rather than
    /// shipping an allowlist parser that accepts `host = ""`.
    #[test]
    fn network_service_validate_rejects_empty_and_bare_wildcard_hosts() {
        let empty = NetworkService {
            host: String::new(),
            ports: vec![443],
            protocol: None,
        };
        assert_eq!(
            empty.validate(),
            Err(NetworkServiceValidationError::EmptyHost),
        );

        let wildcard = NetworkService {
            host: "*".to_string(),
            ports: vec![443],
            protocol: None,
        };
        assert_eq!(
            wildcard.validate(),
            Err(NetworkServiceValidationError::BareWildcardHost),
        );
    }

    /// An empty `ports` list silently allows every destination port,
    /// which includes high-sensitivity ports (22 / SSH, 3389 / RDP).
    /// `validate()` must reject the empty-ports form so users have to
    /// opt in to those ports explicitly. Pin both the rejection AND
    /// the offending port surfaced in the error so a future relaxation
    /// of the list cannot drop SSH protection silently.
    #[test]
    fn network_service_validate_rejects_empty_ports_when_high_sensitivity_implied() {
        let no_ports = NetworkService {
            host: "bastion.example.com".to_string(),
            ports: vec![],
            protocol: None,
        };
        let err = no_ports
            .validate()
            .expect_err("empty ports must be rejected");
        match err {
            NetworkServiceValidationError::HighSensitivityPortRequiresExplicitList {
                host,
                port,
            } => {
                assert_eq!(host, "bastion.example.com");
                // Port 22 is the first high-sensitivity port returned
                // by the validator; the exact port asserted is part
                // of the rejection's diagnostic shape so the user can
                // see which gate fired.
                assert_eq!(port, 22);
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    /// Well-formed services pass validation: explicit hostname,
    /// non-empty ports list, protocol either set or defaulted.
    #[test]
    fn network_service_validate_accepts_well_formed_entries() {
        let https = NetworkService {
            host: "registry.npmjs.org".to_string(),
            ports: vec![443],
            protocol: None,
        };
        assert_eq!(https.validate(), Ok(()));
        assert_eq!(https.effective_protocol(), NetworkProtocol::Tcp);

        let ssh_explicit = NetworkService {
            host: "github.com".to_string(),
            ports: vec![22, 443],
            protocol: Some(NetworkProtocol::Tcp),
        };
        assert_eq!(ssh_explicit.validate(), Ok(()));

        let quic = NetworkService {
            host: "*.example.com".to_string(),
            ports: vec![443],
            protocol: Some(NetworkProtocol::Udp),
        };
        assert_eq!(quic.validate(), Ok(()));
        assert_eq!(quic.effective_protocol(), NetworkProtocol::Udp);
    }

    /// `NetworkService` must round-trip through serde with both the
    /// minimal form (`{host, ports}`, protocol omitted) and the
    /// fully-specified form. The minimal form is what
    /// `.libra/sandbox.toml` will produce; pin the parser-friendly
    /// shape so a future `serde(default)` change doesn't silently
    /// require `protocol` in the TOML.
    #[test]
    fn network_service_serde_round_trips_minimal_and_explicit_forms() {
        let minimal = NetworkService {
            host: "registry.npmjs.org".to_string(),
            ports: vec![443],
            protocol: None,
        };
        let serialised = serde_json::to_string(&minimal).unwrap();
        assert!(
            !serialised.contains("protocol"),
            "minimal form must skip protocol when None; got {serialised}",
        );
        let back: NetworkService = serde_json::from_str(&serialised).unwrap();
        assert_eq!(back, minimal);

        let explicit = NetworkService {
            host: "*.example.com".to_string(),
            ports: vec![443],
            protocol: Some(NetworkProtocol::Udp),
        };
        let serialised = serde_json::to_string(&explicit).unwrap();
        let back: NetworkService = serde_json::from_str(&serialised).unwrap();
        assert_eq!(back, explicit);
    }

    /// Three-state `NetworkAccess` partition: every variant must
    /// satisfy `is_enabled() XOR is_denied()`. `Allowlist` and `Full`
    /// both count as "enabled" (the disable-network env gate fires
    /// only on `Denied`); only `Denied` satisfies `is_denied`.
    #[test]
    fn network_access_three_state_predicates_partition() {
        for mode in [
            NetworkAccess::Denied,
            NetworkAccess::Allowlist {
                services: vec![NetworkService {
                    host: "registry.npmjs.org".to_string(),
                    ports: vec![443],
                    protocol: None,
                }],
            },
            NetworkAccess::Full,
        ] {
            assert_eq!(
                mode.is_enabled(),
                !mode.is_denied(),
                "is_enabled / is_denied must partition for {mode:?}",
            );
        }
        assert_eq!(NetworkAccess::default(), NetworkAccess::Denied);
    }

    /// `is_full` / `is_allowlist` / `is_denied` mutual exclusivity:
    /// exactly one is true for any variant. Pins the three-state
    /// contract at the helper level so a future variant (Phase 7+)
    /// fails to compile until the helpers are extended.
    #[test]
    fn network_access_variant_helpers_are_mutually_exclusive() {
        let denied = NetworkAccess::Denied;
        assert!(denied.is_denied() && !denied.is_allowlist() && !denied.is_full());

        let allowlist = NetworkAccess::Allowlist {
            services: Vec::new(),
        };
        assert!(!allowlist.is_denied() && allowlist.is_allowlist() && !allowlist.is_full());

        let full = NetworkAccess::Full;
        assert!(!full.is_denied() && !full.is_allowlist() && full.is_full());
    }

    /// Wire tags round-trip for the three-state form. The tagged
    /// representation uses `mode: "denied" | "allowlist" | "full"`
    /// — pin the kebab-case names so a future rename trips loud.
    #[test]
    fn network_access_three_state_wire_tags_round_trip() {
        let denied = NetworkAccess::Denied;
        let denied_wire = serde_json::to_string(&denied).unwrap();
        assert!(
            denied_wire.contains("\"denied\""),
            "denied wire must include `denied`; got {denied_wire}",
        );
        let back: NetworkAccess = serde_json::from_str(&denied_wire).unwrap();
        assert_eq!(back, denied);

        let allowlist = NetworkAccess::Allowlist {
            services: vec![NetworkService {
                host: "registry.npmjs.org".to_string(),
                ports: vec![443],
                protocol: None,
            }],
        };
        let allowlist_wire = serde_json::to_string(&allowlist).unwrap();
        assert!(allowlist_wire.contains("\"allowlist\""));
        let back: NetworkAccess = serde_json::from_str(&allowlist_wire).unwrap();
        assert_eq!(back, allowlist);

        let full = NetworkAccess::Full;
        let full_wire = serde_json::to_string(&full).unwrap();
        assert!(full_wire.contains("\"full\""));
        let back: NetworkAccess = serde_json::from_str(&full_wire).unwrap();
        assert_eq!(back, full);
    }

    /// `from_legacy_bool` preserves the `.libra/sandbox.toml`
    /// `network_access = true|false` parser semantics: `true → Full`,
    /// `false → Denied`. Pins the migration adapter so a future
    /// rename can't drop the boolean code path silently.
    #[test]
    fn network_access_from_legacy_bool_maps_to_full_or_denied() {
        assert_eq!(NetworkAccess::from_legacy_bool(true), NetworkAccess::Full);
        assert_eq!(
            NetworkAccess::from_legacy_bool(false),
            NetworkAccess::Denied
        );
    }

    /// Legacy JSON form `network_access: true | false` must deserialize
    /// straight into the three-state enum so older
    /// `.libra/sandbox.toml` configs and persisted `SandboxPolicy`
    /// envelopes from before the Phase 7 migration keep loading.
    /// Pins `docs/development/commands/sandbox.md` §7.1 line 305 and the §
    /// "验证方式" integration test item 9 contract.
    #[test]
    fn network_access_deserialize_accepts_legacy_bool_form() {
        let full: NetworkAccess =
            serde_json::from_str("true").expect("legacy `true` must deserialize");
        assert_eq!(full, NetworkAccess::Full);

        let denied: NetworkAccess =
            serde_json::from_str("false").expect("legacy `false` must deserialize");
        assert_eq!(denied, NetworkAccess::Denied);
    }

    /// `SandboxPolicy::WorkspaceWrite { network_access: true | false }`
    /// from older Codex-style envelopes must round-trip through the
    /// three-state enum without serde errors. Pins the second half of
    /// sandbox.md §7.1 integration test item 9 ("迁移兼容").
    #[test]
    fn sandbox_policy_workspace_write_accepts_legacy_network_access_bool() {
        let legacy_full = r#"{"type": "workspace-write", "network_access": true}"#;
        let parsed: SandboxPolicy =
            serde_json::from_str(legacy_full).expect("legacy `true` payload must parse");
        match parsed {
            SandboxPolicy::WorkspaceWrite { network_access, .. } => {
                assert_eq!(network_access, NetworkAccess::Full);
            }
            other => panic!("expected WorkspaceWrite, got {other:?}"),
        }

        let legacy_denied = r#"{"type": "workspace-write", "network_access": false}"#;
        let parsed: SandboxPolicy =
            serde_json::from_str(legacy_denied).expect("legacy `false` payload must parse");
        match parsed {
            SandboxPolicy::WorkspaceWrite { network_access, .. } => {
                assert_eq!(network_access, NetworkAccess::Denied);
            }
            other => panic!("expected WorkspaceWrite, got {other:?}"),
        }
    }

    /// The custom `Deserialize` must still accept the modern tagged
    /// form (the derived `Serialize` writes this shape). Belt-and-
    /// suspenders pin in addition to
    /// [`network_access_three_state_wire_tags_round_trip`] so a future
    /// refactor of the manual `Deserialize` impl can't silently break
    /// the canonical wire form.
    #[test]
    fn network_access_deserialize_accepts_tagged_form() {
        let denied: NetworkAccess =
            serde_json::from_str(r#"{"mode": "denied"}"#).expect("tagged denied must parse");
        assert_eq!(denied, NetworkAccess::Denied);

        let full: NetworkAccess =
            serde_json::from_str(r#"{"mode": "full"}"#).expect("tagged full must parse");
        assert_eq!(full, NetworkAccess::Full);

        let allowlist: NetworkAccess = serde_json::from_str(
            r#"{"mode": "allowlist", "services": [{"host": "registry.npmjs.org", "ports": [443]}]}"#,
        )
        .expect("tagged allowlist must parse");
        match allowlist {
            NetworkAccess::Allowlist { services } => {
                assert_eq!(services.len(), 1);
                assert_eq!(services[0].host, "registry.npmjs.org");
            }
            other => panic!("expected Allowlist, got {other:?}"),
        }
    }

    /// `SandboxPolicy::ExternalSandbox { network_access: true | false }`
    /// envelopes from the pre-Phase-7 wire shape must also accept the
    /// legacy bool form, not just `WorkspaceWrite`. Both variants share
    /// the same `network_access: NetworkAccess` field, so the migration
    /// contract must hold for both. Pin so a future refactor that
    /// tightens one variant but forgets the other is caught here.
    #[test]
    fn sandbox_policy_external_sandbox_accepts_legacy_network_access_bool() {
        let legacy_full = r#"{"type": "external-sandbox", "network_access": true}"#;
        let parsed: SandboxPolicy =
            serde_json::from_str(legacy_full).expect("external-sandbox legacy `true` must parse");
        match parsed {
            SandboxPolicy::ExternalSandbox { network_access } => {
                assert_eq!(network_access, NetworkAccess::Full);
            }
            other => panic!("expected ExternalSandbox, got {other:?}"),
        }

        let legacy_denied = r#"{"type": "external-sandbox", "network_access": false}"#;
        let parsed: SandboxPolicy = serde_json::from_str(legacy_denied)
            .expect("external-sandbox legacy `false` must parse");
        match parsed {
            SandboxPolicy::ExternalSandbox { network_access } => {
                assert_eq!(network_access, NetworkAccess::Denied);
            }
            other => panic!("expected ExternalSandbox, got {other:?}"),
        }
    }

    /// Inputs that are neither bool nor a `{mode: …}` object must
    /// fail to deserialize — otherwise an accidentally-accepted shape
    /// (e.g., a bare string like `"full"`, which was never part of
    /// either the legacy or current wire contract) would silently land
    /// the wrong variant.
    #[test]
    fn network_access_deserialize_rejects_non_contract_shapes() {
        for bad_input in [
            "null",
            "0",
            "1",
            "\"full\"",
            "\"denied\"",
            "[]",
            r#"{"mode": "unknown"}"#,
            r#"{"not_mode": "denied"}"#,
        ] {
            assert!(
                serde_json::from_str::<NetworkAccess>(bad_input).is_err(),
                "expected NetworkAccess to reject malformed input `{bad_input}`",
            );
        }
    }

    /// Off-contract type errors must surface the visitor's `expecting`
    /// text so users see actionable guidance ("either a legacy boolean
    /// ... or a tagged object {mode: ...}") rather than the generic
    /// `untagged enum did not match any variant` message that the
    /// previous derived form produced. Pin against the three most
    /// likely user-facing shapes: `null`, a bare string, and a number.
    #[test]
    fn network_access_deserialize_error_messages_include_visitor_expecting_hint() {
        for bad_input in ["null", "\"full\"", "42", "[]"] {
            let error = serde_json::from_str::<NetworkAccess>(bad_input)
                .expect_err("malformed input should not deserialize");
            let message = error.to_string();
            assert!(
                message.contains("legacy boolean") && message.contains("tagged object"),
                "error message for `{bad_input}` should include the visitor's \
                 `expecting` hint about legacy boolean / tagged object, got: {message}",
            );
        }
    }

    fn svc(host: &str, ports: Vec<u16>) -> NetworkService {
        NetworkService {
            host: host.to_string(),
            ports,
            protocol: None,
        }
    }

    /// `restrict_with` is tightening-only: when the two inputs have
    /// different restrictiveness ranks (`Denied < Allowlist < Full`),
    /// the lower-ranked (more locked-down) value always wins, carrying
    /// its own services when it is the `Allowlist`. Critically, a
    /// config `Full` applied to a `Denied` policy must NOT loosen it —
    /// that upgrade requires the approval channel (sandbox.md §7.5).
    #[test]
    fn restrict_with_returns_more_restrictive_across_ranks() {
        let allowlist = NetworkAccess::Allowlist {
            services: vec![svc("registry.npmjs.org", vec![443])],
        };

        // Config tightens Full → Allowlist (config's services define the set).
        assert_eq!(NetworkAccess::Full.restrict_with(&allowlist), allowlist);
        // Symmetric: a policy allowlist narrowed by a config Full stays the allowlist.
        assert_eq!(allowlist.restrict_with(&NetworkAccess::Full), allowlist);

        // Denied always wins regardless of the other side.
        assert_eq!(
            NetworkAccess::Full.restrict_with(&NetworkAccess::Denied),
            NetworkAccess::Denied
        );
        assert_eq!(
            allowlist.restrict_with(&NetworkAccess::Denied),
            NetworkAccess::Denied
        );

        // A config `Full` can NEVER widen a more-restrictive policy.
        assert_eq!(
            NetworkAccess::Denied.restrict_with(&NetworkAccess::Full),
            NetworkAccess::Denied
        );
        assert_eq!(
            allowlist.restrict_with(&NetworkAccess::Full),
            allowlist,
            "config Full must not loosen a policy allowlist to Full",
        );

        // A config allowlist with NO services narrowing a Full policy
        // is deny-all, not "allow everything" — must collapse to Denied.
        let empty_allowlist = NetworkAccess::Allowlist { services: vec![] };
        assert_eq!(
            NetworkAccess::Full.restrict_with(&empty_allowlist),
            NetworkAccess::Denied,
            "an empty config allowlist must lock a Full policy down to Denied",
        );
    }

    /// Two `Allowlist`s combine to the **intersection** of their
    /// service lists, which is a subset of each input — so the result
    /// can never grant a host that only one side allowed. An empty
    /// intersection collapses to `Denied` (deny-all), never an empty
    /// `Allowlist`.
    #[test]
    fn restrict_with_intersects_two_allowlists() {
        let policy = NetworkAccess::Allowlist {
            services: vec![
                svc("a.example.com", vec![443]),
                svc("b.example.com", vec![443]),
            ],
        };
        let config = NetworkAccess::Allowlist {
            services: vec![
                svc("b.example.com", vec![443]),
                svc("c.example.com", vec![443]),
            ],
        };
        // Only `b` (identical host+ports) is in both lists.
        assert_eq!(
            policy.restrict_with(&config),
            NetworkAccess::Allowlist {
                services: vec![svc("b.example.com", vec![443])],
            }
        );

        // Differing ports = different service = excluded from the
        // intersection. The resulting empty allowlist collapses to
        // Denied (deny-all) rather than an empty `Allowlist`.
        let port_mismatch = NetworkAccess::Allowlist {
            services: vec![svc("b.example.com", vec![8443])],
        };
        assert_eq!(
            policy.restrict_with(&port_mismatch),
            NetworkAccess::Denied,
            "an empty intersection must collapse to Denied, never an empty allowlist",
        );

        // A disjoint config allowlist (no shared host) also collapses to Denied.
        let disjoint = NetworkAccess::Allowlist {
            services: vec![svc("z.example.com", vec![443])],
        };
        assert_eq!(policy.restrict_with(&disjoint), NetworkAccess::Denied);

        // Both Denied / both Full collapse to that shared value.
        assert_eq!(
            NetworkAccess::Denied.restrict_with(&NetworkAccess::Denied),
            NetworkAccess::Denied
        );
        assert_eq!(
            NetworkAccess::Full.restrict_with(&NetworkAccess::Full),
            NetworkAccess::Full
        );
    }

    /// `with_network_restriction` tightens only the network-bearing
    /// variants. `ReadOnly` (already `Denied`) and `DangerFullAccess`
    /// (explicit host-level escalation with no `network_access` field)
    /// are returned unchanged so a config file cannot silently mutate
    /// an opt-out-of-sandbox decision.
    #[test]
    fn with_network_restriction_only_touches_network_bearing_variants() {
        let workspace = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![PathBuf::from("/tmp/ws")],
            network_access: NetworkAccess::Full,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: false,
        };
        match workspace.with_network_restriction(&NetworkAccess::Denied) {
            SandboxPolicy::WorkspaceWrite {
                writable_roots,
                network_access,
                exclude_tmpdir_env_var,
                exclude_slash_tmp,
            } => {
                assert_eq!(network_access, NetworkAccess::Denied);
                // Non-network fields must be preserved verbatim.
                assert_eq!(writable_roots, vec![PathBuf::from("/tmp/ws")]);
                assert!(exclude_tmpdir_env_var);
                assert!(!exclude_slash_tmp);
            }
            other => panic!("expected WorkspaceWrite, got {other:?}"),
        }

        let external = SandboxPolicy::ExternalSandbox {
            network_access: NetworkAccess::Full,
        };
        assert_eq!(
            external.with_network_restriction(&NetworkAccess::Denied),
            SandboxPolicy::ExternalSandbox {
                network_access: NetworkAccess::Denied
            },
        );

        // ReadOnly and DangerFullAccess pass through untouched.
        assert_eq!(
            SandboxPolicy::ReadOnly.with_network_restriction(&NetworkAccess::Full),
            SandboxPolicy::ReadOnly,
        );
        assert_eq!(
            SandboxPolicy::DangerFullAccess.with_network_restriction(&NetworkAccess::Denied),
            SandboxPolicy::DangerFullAccess,
        );
    }
}
