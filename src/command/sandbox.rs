//! `libra sandbox` diagnostics command surface.

use std::{
    env,
    io::Write,
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::{
    internal::ai::sandbox::{
        NetworkAccess, NetworkAccessMode, NetworkProtocol, NetworkProxy, NetworkProxySelection,
        NetworkService, ProxyEnforcement, SandboxEnforcement, SandboxPolicy,
        allowlist_proxy_from_policy, load_sandbox_config_network_access, select_network_proxy,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
    },
};

const SANDBOX_ENFORCEMENT_ENV: &str = "LIBRA_SANDBOX_ENFORCEMENT";
const LINUX_SANDBOX_EXE_ENV: &str = "LIBRA_LINUX_SANDBOX_EXE";
const LINUX_SANDBOX_BWRAP_ENV: &str = "LIBRA_USE_LINUX_SANDBOX_BWRAP";
const MACOS_SEATBELT_EXECUTABLE: &str = "/usr/bin/sandbox-exec";

/// `--help` examples shown in `libra sandbox --help` output.
///
/// `sandbox` today only exposes the `status` sub-command, which prints
/// the effective sandbox diagnostics for AI tool execution. The banner
/// pins the human, JSON, and machine-mode forms so users see the three
/// supported invocations without reading the design doc. Cross-cutting
/// `--help` EXAMPLES rollout per `docs/development/commands/_general.md` item B.
pub const SANDBOX_EXAMPLES: &str = "\
EXAMPLES:
    libra sandbox status            Show effective sandbox diagnostics for AI tool execution
    libra sandbox --json status     Structured JSON output for agents
    libra sandbox --machine status  Machine-strict JSON (implies --json=ndjson --no-pager --quiet)";

#[derive(Parser, Debug)]
#[command(after_help = SANDBOX_EXAMPLES)]
pub struct SandboxArgs {
    #[command(subcommand)]
    pub command: SandboxSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum SandboxSubcommand {
    /// Show effective sandbox diagnostics for AI tool execution.
    Status,
}

#[derive(Debug, Serialize)]
struct SandboxStatusOutput {
    platform: &'static str,
    sandbox_type: &'static str,
    enforcement: &'static str,
    effective_enforcement: &'static str,
    writable_roots: Vec<String>,
    network: SandboxNetworkStatus,
    proxy_backend: &'static str,
    bwrap_available: bool,
    bwrap_requested: bool,
    seatbelt_available: bool,
    helper_path: SandboxHelperPathStatus,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SandboxNetworkStatus {
    mode: &'static str,
    allowlist: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SandboxHelperPathStatus {
    path: Option<String>,
    exists: bool,
}

pub async fn execute_safe(args: SandboxArgs, output: &OutputConfig) -> CliResult<()> {
    match args.command {
        SandboxSubcommand::Status => status(output).await,
    }
}

async fn status(output: &OutputConfig) -> CliResult<()> {
    let report = build_status_report()?;
    if output.is_json() {
        return emit_json_data("sandbox.status", &report, output);
    }
    render_status_human(&report, output)
}

fn build_status_report() -> CliResult<SandboxStatusOutput> {
    let cwd = env::current_dir().map_err(|error| {
        CliError::io(format!(
            "failed to inspect sandbox status from current directory: {error}"
        ))
    })?;
    let policy = effective_status_policy(&cwd)?;
    let writable_roots = policy
        .get_writable_roots_with_cwd(&cwd)
        .into_iter()
        .map(|root| root.root.display().to_string())
        .collect();
    let helper_path = env_path(LINUX_SANDBOX_EXE_ENV);
    let helper_exists = helper_path.as_deref().is_some_and(executable_file_exists);
    let bwrap_available = locate_bwrap_binary_for_status().is_some();
    let bwrap_requested = env_flag_enabled(LINUX_SANDBOX_BWRAP_ENV);
    let seatbelt_available = executable_file_exists(Path::new(MACOS_SEATBELT_EXECUTABLE));
    let mut warnings = Vec::new();
    let enforcement = env_sandbox_enforcement(&mut warnings);
    let (network_mode, allowlist, proxy_backend, network_warning) =
        describe_network_access(&policy, enforcement);
    if let Some(reason) = network_warning {
        warnings.push(reason);
    }
    warnings.extend(host_environment_warnings());

    let sandbox_type = match env::consts::OS {
        "macos" if seatbelt_available => "macos-seatbelt",
        "macos" => {
            warnings.push(
                "macOS sandbox-exec is not available; AI shell commands cannot use Seatbelt"
                    .to_string(),
            );
            "none"
        }
        "linux" if helper_exists || bwrap_available => {
            if !helper_exists {
                if let Some(path) = &helper_path {
                    warnings.push(format!(
                        "linux sandbox helper '{}' is not executable; using built-in bwrap sandbox instead",
                        path.display()
                    ));
                } else {
                    warnings.push(
                        "linux sandbox helper is not configured; using built-in bwrap as internal sandbox backend".to_string(),
                    );
                }
            }
            "linux-seccomp"
        }
        "linux" => {
            if let Some(path) = &helper_path {
                if enforcement.requires_effective_sandbox() {
                    warnings.push(format!(
                        "linux sandbox helper '{}' is not executable and built-in bwrap is not available; AI shell commands that require Libra's internal sandbox will fail",
                        path.display()
                    ));
                } else {
                    warnings.push(format!(
                        "linux sandbox helper '{}' is not executable and built-in bwrap is not available; AI shell commands currently fall back to no OS sandbox",
                        path.display()
                    ));
                }
            } else {
                if enforcement.requires_effective_sandbox() {
                    warnings.push(
                        "linux sandbox helper is not configured and bwrap is unavailable; AI shell commands that require Libra's internal sandbox will fail"
                            .to_string(),
                    );
                } else {
                    warnings.push(
                        "linux sandbox helper is not configured and bwrap is unavailable; AI shell commands currently fall back to no OS sandbox"
                            .to_string(),
                    );
                }
            }
            "none"
        }
        "windows" => {
            warnings.push(
                "Windows restricted-token sandbox is not implemented in the current runtime"
                    .to_string(),
            );
            "windows-restricted-token-unimplemented"
        }
        _ => {
            warnings.push(format!(
                "OS sandbox execution is not supported on platform '{}'",
                env::consts::OS
            ));
            "none"
        }
    };

    if bwrap_requested && !bwrap_available {
        warnings.push(
            "LIBRA_USE_LINUX_SANDBOX_BWRAP is enabled but bwrap is not available on PATH"
                .to_string(),
        );
    }

    Ok(SandboxStatusOutput {
        platform: env::consts::OS,
        sandbox_type,
        enforcement: enforcement.as_str(),
        effective_enforcement: enforcement.as_str(),
        writable_roots,
        network: SandboxNetworkStatus {
            mode: network_mode,
            allowlist,
        },
        proxy_backend,
        bwrap_available,
        bwrap_requested,
        seatbelt_available,
        helper_path: SandboxHelperPathStatus {
            path: helper_path.map(|path| path.display().to_string()),
            exists: helper_exists,
        },
        warnings,
    })
}

fn effective_status_policy(cwd: &Path) -> CliResult<SandboxPolicy> {
    let policy = SandboxPolicy::default();
    let config_network_access =
        load_sandbox_config_network_access(cwd).map_err(sandbox_status_config_error)?;
    let Some(config_network_access) = config_network_access else {
        return Ok(policy);
    };
    Ok(policy.with_network_restriction(&config_network_access))
}

fn sandbox_status_config_error(error: String) -> CliError {
    CliError::fatal(error)
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint("fix .libra/sandbox.toml or remove the invalid [sandbox.network] section.")
}

fn render_status_human(report: &SandboxStatusOutput, output: &OutputConfig) -> CliResult<()> {
    if !output.quiet {
        // Build the rendered block as a string up front so we have a
        // single source-of-truth shape that both production (`stdout`)
        // and the unit tests can consume. The pure-function form is
        // covered by `format_status_human` tests in this module.
        print!("{}", format_status_human(report));
    }
    std::io::stdout()
        .flush()
        .map_err(|error| CliError::io(format!("failed to write sandbox status: {error}")))?;
    Ok(())
}

/// Render the human form of [`SandboxStatusOutput`] as a single string
/// (each line terminated by `\n`).
///
/// Extracted from [`render_status_human`] so the rendering shape can
/// be unit-tested without redirecting the process stdout — which is
/// fragile under `cargo test`'s default libtest output capture
/// (`BufferRedirect` taps the raw FD but libtest wraps stdout in its
/// own capture buffer, so the two layers don't compose reliably).
fn format_status_human(report: &SandboxStatusOutput) -> String {
    use std::fmt::Write;

    let mut buffer = String::new();
    let _ = writeln!(buffer, "Sandbox status");
    let _ = writeln!(buffer, "  platform: {}", report.platform);
    let _ = writeln!(buffer, "  sandbox_type: {}", report.sandbox_type);
    let _ = writeln!(buffer, "  enforcement: {}", report.enforcement);
    let _ = writeln!(
        buffer,
        "  effective_enforcement: {}",
        report.effective_enforcement
    );
    let _ = writeln!(buffer, "  network: {}", report.network.mode);
    if !report.network.allowlist.is_empty() {
        // Allowlist entries are only populated when `network.mode ==
        // "allowlist"` (see `describe_network_access`). Mirror the
        // JSON `data.network.allowlist` field so human readers see
        // which services the proxy will actually route — otherwise
        // they have to switch to `--json` to learn what `allowlist`
        // means for this invocation, which contradicts
        // `docs/development/commands/sandbox.md` §7.4 line 350 (`libra sandbox
        // status` must output `network.allowlist`).
        let _ = writeln!(buffer, "  network_allowlist:");
        for entry in &report.network.allowlist {
            let _ = writeln!(buffer, "    - {entry}");
        }
    }
    let _ = writeln!(buffer, "  proxy_backend: {}", report.proxy_backend);
    let _ = writeln!(buffer, "  bwrap_available: {}", report.bwrap_available);
    let _ = writeln!(buffer, "  bwrap_requested: {}", report.bwrap_requested);
    let _ = writeln!(
        buffer,
        "  seatbelt_available: {}",
        report.seatbelt_available
    );
    let helper_path = report
        .helper_path
        .path
        .as_deref()
        .unwrap_or("(not configured)");
    let _ = writeln!(buffer, "  helper_path: {helper_path}");
    let _ = writeln!(
        buffer,
        "  helper_path_exists: {}",
        report.helper_path.exists
    );
    let _ = writeln!(buffer, "  writable_roots:");
    for root in &report.writable_roots {
        let _ = writeln!(buffer, "    - {root}");
    }
    if !report.warnings.is_empty() {
        let _ = writeln!(buffer, "  warnings:");
        for warning in &report.warnings {
            let _ = writeln!(buffer, "    - {warning}");
        }
    }
    buffer
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name).and_then(|value| {
        if value.to_string_lossy().is_empty() {
            None
        } else {
            Some(PathBuf::from(value))
        }
    })
}

fn env_flag_enabled(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| {
        let value = value.to_string_lossy().to_ascii_lowercase();
        matches!(value.as_str(), "1" | "true" | "yes" | "on")
    })
}

fn env_sandbox_enforcement(warnings: &mut Vec<String>) -> SandboxEnforcement {
    let Ok(value) = env::var(SANDBOX_ENFORCEMENT_ENV) else {
        return SandboxEnforcement::default();
    };

    match value.parse::<SandboxEnforcement>() {
        Ok(enforcement) => enforcement,
        Err(error) => {
            warnings.push(error.to_string());
            SandboxEnforcement::default()
        }
    }
}

#[derive(Debug)]
struct HostEnvironmentSignals {
    os: &'static str,
    wsl_distro_name: Option<String>,
    proc_version: Option<String>,
    docker_env_file_exists: bool,
    container_env_file_exists: bool,
    proc_1_cgroup: Option<String>,
}

fn host_environment_warnings() -> Vec<String> {
    let wsl_distro_name = env::var("WSL_DISTRO_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let proc_version = std::fs::read_to_string("/proc/version").ok();
    let proc_1_cgroup = std::fs::read_to_string("/proc/1/cgroup").ok();
    sandbox_host_environment_warnings(&HostEnvironmentSignals {
        os: env::consts::OS,
        wsl_distro_name,
        proc_version,
        docker_env_file_exists: Path::new("/.dockerenv").exists(),
        container_env_file_exists: Path::new("/run/.containerenv").exists(),
        proc_1_cgroup,
    })
}

fn sandbox_host_environment_warnings(signals: &HostEnvironmentSignals) -> Vec<String> {
    if signals.os != "linux" {
        return Vec::new();
    }

    let mut warnings = Vec::new();
    if is_wsl_environment(signals) {
        warnings.push(
            "WSL environment detected; AI sandbox isolation may be constrained by the Windows host/WSL boundary. Verify `libra sandbox status` before relying on OS-level isolation."
                .to_string(),
        );
    }
    if is_container_environment(signals) {
        warnings.push(
            "containerized environment detected; AI sandbox isolation may be constrained by the outer container namespace and mount policy."
                .to_string(),
        );
    }
    warnings
}

fn is_wsl_environment(signals: &HostEnvironmentSignals) -> bool {
    signals.wsl_distro_name.is_some()
        || signals.proc_version.as_deref().is_some_and(|version| {
            let version = version.to_ascii_lowercase();
            version.contains("microsoft") || version.contains("wsl")
        })
}

fn is_container_environment(signals: &HostEnvironmentSignals) -> bool {
    if signals.docker_env_file_exists || signals.container_env_file_exists {
        return true;
    }

    signals.proc_1_cgroup.as_deref().is_some_and(|cgroup| {
        let cgroup = cgroup.to_ascii_lowercase();
        ["docker", "containerd", "kubepods", "libpod", "podman"]
            .iter()
            .any(|marker| cgroup.contains(marker))
    })
}

#[cfg(not(target_os = "linux"))]
fn locate_bwrap_binary_for_status() -> Option<PathBuf> {
    None
}

#[cfg(target_os = "linux")]
fn locate_bwrap_binary_for_status() -> Option<PathBuf> {
    if let Some(override_path) = env::var_os("LIBRA_BWRAP_BINARY") {
        let path = PathBuf::from(override_path);
        if path.is_absolute() && executable_file_exists(&path) {
            return Some(path);
        }
        return None;
    }

    let path_env = env::var_os("PATH")?;
    for dir in env::split_paths(&path_env) {
        let candidate = dir.join("bwrap");
        if executable_file_exists(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn describe_network_access(
    policy: &SandboxPolicy,
    enforcement: SandboxEnforcement,
) -> (&'static str, Vec<String>, &'static str, Option<String>) {
    let network_access = current_network_access(policy);
    let mode = match network_access {
        NetworkAccess::Denied => NetworkAccessMode::Denied,
        NetworkAccess::Allowlist { .. } => NetworkAccessMode::Allowlist,
        NetworkAccess::Full => NetworkAccessMode::Full,
    };
    let mode_name = match network_access {
        NetworkAccess::Denied => "denied",
        NetworkAccess::Allowlist { .. } => "allowlist",
        NetworkAccess::Full => "full",
    };
    let (allowlist_proxy, allowlist_proxy_error) = match allowlist_proxy_from_policy(policy) {
        Ok(proxy) => (proxy, None),
        Err(reason) => (None, Some(reason)),
    };

    let proxy = match (mode, allowlist_proxy_error) {
        (NetworkAccessMode::Allowlist, Some(reason)) => match enforcement {
            SandboxEnforcement::Required => NetworkProxySelection::Reject {
                reason: format!(
                    "NetworkAccess::Allowlist requested but the per-allowlist proxy is unavailable: {reason}; SandboxEnforcement::Required forbids degrading to Denied",
                ),
            },
            SandboxEnforcement::PreferStrict => NetworkProxySelection::DegradeToDenied {
                reason: format!(
                    "NetworkAccess::Allowlist requested but proxy unavailable: {reason}; degrading to Denied under SandboxEnforcement::PreferStrict",
                ),
            },
            SandboxEnforcement::BestEffort => NetworkProxySelection::DegradeToDenied {
                reason: format!(
                    "NetworkAccess::Allowlist requested but proxy unavailable: {reason}; silently degrading to Denied under SandboxEnforcement::BestEffort",
                ),
            },
        },
        (_, _) => select_network_proxy(
            mode,
            allowlist_proxy
                .as_ref()
                .map(|proxy| proxy as &dyn NetworkProxy),
            enforcement.into(),
        ),
    };
    let (proxy_backend, network_warning) = match proxy {
        NetworkProxySelection::Proxy(proxy) => (proxy.backend_name(), None),
        NetworkProxySelection::DegradeToDenied { reason } => ("none", Some(reason)),
        NetworkProxySelection::Reject { reason } => ("none", Some(reason)),
    };
    let allowlist = network_access
        .allowlist_services()
        .unwrap_or_default()
        .iter()
        .map(format_network_service)
        .collect();
    (mode_name, allowlist, proxy_backend, network_warning)
}

fn current_network_access(policy: &SandboxPolicy) -> NetworkAccess {
    match policy {
        SandboxPolicy::DangerFullAccess => NetworkAccess::Full,
        SandboxPolicy::ReadOnly => NetworkAccess::Denied,
        SandboxPolicy::ExternalSandbox { network_access }
        | SandboxPolicy::WorkspaceWrite { network_access, .. } => network_access.clone(),
    }
}

fn format_network_service(service: &NetworkService) -> String {
    let ports = if service.ports.is_empty() {
        "any".to_string()
    } else {
        service
            .ports
            .iter()
            .map(|port| port.to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    let protocol = match service.protocol.unwrap_or(NetworkProtocol::Tcp) {
        NetworkProtocol::Tcp => "tcp",
        NetworkProtocol::Udp => "udp",
    };
    format!("{}:{ports}:{protocol}", service.host)
}

fn executable_file_exists(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        path.metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

impl From<SandboxEnforcement> for ProxyEnforcement {
    fn from(value: SandboxEnforcement) -> Self {
        match value {
            SandboxEnforcement::Required => ProxyEnforcement::Required,
            SandboxEnforcement::PreferStrict => ProxyEnforcement::PreferStrict,
            SandboxEnforcement::BestEffort => ProxyEnforcement::BestEffort,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HostEnvironmentSignals, SandboxHelperPathStatus, SandboxNetworkStatus, SandboxStatusOutput,
        format_status_human, sandbox_host_environment_warnings,
    };

    /// Build a `SandboxStatusOutput` fixture for the human-render
    /// assertions. Defaults mirror the macOS Seatbelt sandbox with
    /// the `Denied` network mode — individual tests override the
    /// fields they exercise.
    fn fixture(allowlist: Vec<String>) -> SandboxStatusOutput {
        SandboxStatusOutput {
            platform: "macos",
            sandbox_type: "macos-seatbelt",
            enforcement: "best_effort",
            effective_enforcement: "best_effort",
            writable_roots: vec!["/repo".to_string()],
            network: SandboxNetworkStatus {
                mode: if allowlist.is_empty() {
                    "denied"
                } else {
                    "allowlist"
                },
                allowlist,
            },
            proxy_backend: "noop",
            bwrap_available: false,
            bwrap_requested: false,
            seatbelt_available: true,
            helper_path: SandboxHelperPathStatus {
                path: None,
                exists: false,
            },
            warnings: Vec::new(),
        }
    }

    /// `format_status_human` must emit a `network_allowlist:` block
    /// listing every entry in `network.allowlist` when the list is
    /// non-empty. Pin against `docs/development/commands/sandbox.md` §7.4
    /// line 350 ("`libra sandbox status` 输出 ... `network.allowlist`")
    /// and `docs/commands/sandbox.md`'s field table — without this,
    /// the JSON form exposes the allowlist but the human form
    /// silently hides it, forcing users to switch to `--json` to see
    /// which services the proxy actually allows.
    #[test]
    fn format_status_human_emits_network_allowlist_when_present() {
        let rendered = format_status_human(&fixture(vec![
            "registry.npmjs.org:443".to_string(),
            "*.pypi.org:443".to_string(),
        ]));

        assert!(
            rendered.contains("network: allowlist"),
            "human render must include `network: allowlist` line, got:\n{rendered}",
        );
        assert!(
            rendered.contains("network_allowlist:"),
            "human render must include the `network_allowlist:` block when entries exist, got:\n{rendered}",
        );
        for entry in ["registry.npmjs.org:443", "*.pypi.org:443"] {
            assert!(
                rendered.contains(&format!("    - {entry}")),
                "human render must list allowlist entry `{entry}` with the 4-space indent, got:\n{rendered}",
            );
        }
    }

    /// When `network.allowlist` is empty the `network_allowlist:`
    /// block must NOT appear — otherwise denied-mode operators see a
    /// dangling header with no entries. Pin the absence so a future
    /// refactor that always renders the section trips this test.
    #[test]
    fn format_status_human_omits_network_allowlist_when_empty() {
        let rendered = format_status_human(&fixture(Vec::new()));

        assert!(
            rendered.contains("network: denied"),
            "human render must include `network: denied`, got:\n{rendered}",
        );
        assert!(
            !rendered.contains("network_allowlist:"),
            "human render must NOT include `network_allowlist:` when the list is empty, got:\n{rendered}",
        );
    }

    /// The non-allowlist baseline fields must appear in every render
    /// regardless of allowlist content. Pin the bedrock shape so a
    /// future refactor of `format_status_human` that drops a baseline
    /// line trips this test.
    #[test]
    fn format_status_human_renders_baseline_fields() {
        let rendered = format_status_human(&fixture(Vec::new()));
        for marker in [
            "Sandbox status",
            "  platform: macos",
            "  sandbox_type: macos-seatbelt",
            "  enforcement: best_effort",
            "  effective_enforcement: best_effort",
            "  proxy_backend: noop",
            "  bwrap_available: false",
            "  seatbelt_available: true",
            "  helper_path: (not configured)",
            "  writable_roots:",
            "    - /repo",
        ] {
            assert!(
                rendered.contains(marker),
                "human render must include `{marker}`, got:\n{rendered}",
            );
        }
    }

    /// `describe_network_access` in `Denied` mode must report
    /// `("denied", [], "noop", None)` for every enforcement tier.
    /// Pin the default-deny network shape so a future refactor of
    /// the `(mode, proxy, enforcement)` decision tree that
    /// accidentally lets `BestEffort` degrade `Denied` into a more
    /// permissive proxy (e.g. `loopback-only`) trips this test.
    #[test]
    fn describe_network_access_denied_mode_routes_through_noop_proxy() {
        use crate::internal::ai::sandbox::{NetworkAccess, SandboxEnforcement, SandboxPolicy};

        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            network_access: NetworkAccess::Denied,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
            allow_metadata_writes: false,
        };
        for enforcement in SandboxEnforcement::all() {
            let (mode, allowlist, proxy_backend, warning) =
                super::describe_network_access(&policy, enforcement);
            assert_eq!(mode, "denied", "enforcement={enforcement:?}");
            assert!(allowlist.is_empty(), "enforcement={enforcement:?}");
            assert_eq!(proxy_backend, "noop", "enforcement={enforcement:?}");
            assert!(
                warning.is_none(),
                "Denied mode must not produce a warning, got {warning:?} for {enforcement:?}",
            );
        }
    }

    /// `describe_network_access` in `Full` mode must report
    /// `("full", [], "loopback-only", None)` for every enforcement
    /// tier. `Full` routes through the loopback-only placeholder
    /// (sandbox.md §7.4 lines 357-358); pin the placeholder name so
    /// when the future pass-through proxy lands and the backend name
    /// changes, this test fails loudly.
    #[test]
    fn describe_network_access_full_mode_routes_through_loopback_only_placeholder() {
        use crate::internal::ai::sandbox::{NetworkAccess, SandboxEnforcement, SandboxPolicy};

        let policy = SandboxPolicy::ExternalSandbox {
            network_access: NetworkAccess::Full,
        };
        for enforcement in SandboxEnforcement::all() {
            let (mode, allowlist, proxy_backend, warning) =
                super::describe_network_access(&policy, enforcement);
            assert_eq!(mode, "full", "enforcement={enforcement:?}");
            assert!(allowlist.is_empty(), "enforcement={enforcement:?}");
            assert_eq!(
                proxy_backend, "loopback-only",
                "enforcement={enforcement:?}"
            );
            assert!(
                warning.is_none(),
                "Full mode must not produce a warning, got {warning:?} for {enforcement:?}",
            );
        }
    }

    /// `describe_network_access` in `Allowlist` mode with valid
    /// services must report `("allowlist", [services], "allowlist",
    /// None)`. The allowlist proxy is constructed when the policy's
    /// services pass [`NetworkService::validate`]; this test pins
    /// that flow end-to-end through the helper.
    #[test]
    fn describe_network_access_allowlist_mode_with_valid_services_routes_through_allowlist_proxy() {
        use crate::internal::ai::sandbox::{
            NetworkAccess, NetworkProtocol, NetworkService, SandboxEnforcement, SandboxPolicy,
        };

        let services = vec![
            NetworkService {
                host: "registry.npmjs.org".to_string(),
                ports: vec![443],
                protocol: Some(NetworkProtocol::Tcp),
            },
            NetworkService {
                host: "*.pypi.org".to_string(),
                ports: vec![443],
                protocol: None,
            },
        ];
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            network_access: NetworkAccess::Allowlist {
                services: services.clone(),
            },
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
            allow_metadata_writes: false,
        };

        for enforcement in SandboxEnforcement::all() {
            let (mode, allowlist, proxy_backend, warning) =
                super::describe_network_access(&policy, enforcement);
            assert_eq!(mode, "allowlist", "enforcement={enforcement:?}");
            assert_eq!(
                allowlist.len(),
                services.len(),
                "allowlist length must match the configured services for enforcement={enforcement:?}",
            );
            assert!(
                allowlist
                    .iter()
                    .any(|entry| entry.contains("registry.npmjs.org")),
                "allowlist must surface the npm registry entry, got {allowlist:?}",
            );
            assert_eq!(proxy_backend, "allowlist", "enforcement={enforcement:?}");
            assert!(
                warning.is_none(),
                "Allowlist with valid services must not warn, got {warning:?} for {enforcement:?}",
            );
        }
    }

    /// `describe_network_access` in `Allowlist` mode with an empty
    /// services list (proxy unavailable) must degrade according to
    /// the enforcement tier:
    ///
    /// - `Required` → `proxy_backend = "none"` + warning citing
    ///   `Required` forbids degradation.
    /// - `PreferStrict` → `proxy_backend = "none"` + warning citing
    ///   `PreferStrict`.
    /// - `BestEffort` → `proxy_backend = "none"` + warning citing
    ///   `BestEffort`.
    ///
    /// Pin the three-tier degradation contract from sandbox.md §7.4
    /// lines 340-343 ("代理不可用则按 `SandboxEnforcement` 决策"),
    /// so a refactor that swaps tiers (e.g. silently allowing
    /// `Required` to fall back to deny-all) trips this test.
    #[test]
    fn describe_network_access_allowlist_without_services_degrades_per_enforcement_tier() {
        use crate::internal::ai::sandbox::{NetworkAccess, SandboxEnforcement, SandboxPolicy};

        let policy = SandboxPolicy::ExternalSandbox {
            network_access: NetworkAccess::Allowlist {
                services: Vec::new(),
            },
        };

        for (enforcement, expected_text) in [
            (SandboxEnforcement::Required, "Required"),
            (SandboxEnforcement::PreferStrict, "PreferStrict"),
            (SandboxEnforcement::BestEffort, "BestEffort"),
        ] {
            let (mode, allowlist, proxy_backend, warning) =
                super::describe_network_access(&policy, enforcement);
            assert_eq!(mode, "allowlist", "enforcement={enforcement:?}");
            assert!(
                allowlist.is_empty(),
                "empty services list must produce empty allowlist for {enforcement:?}, got {allowlist:?}",
            );
            assert_eq!(
                proxy_backend, "none",
                "unavailable allowlist proxy must map to `none` backend for {enforcement:?}",
            );
            let warning = warning.expect("unavailable allowlist must surface a warning");
            assert!(
                warning.contains(expected_text),
                "warning must cite the `{expected_text}` enforcement tier, got: {warning}",
            );
        }
    }

    /// `format_network_service` is the shape that
    /// `libra sandbox status` surfaces for every allowlist entry —
    /// both in the JSON `data.network.allowlist[]` array and in the
    /// new human `network_allowlist:` block (v0.17.986). Pin the
    /// `"{host}:{ports}:{protocol}"` shape across the five branches
    /// of the formatter (empty / single / multi-port × default /
    /// explicit-TCP / UDP protocol) so a refactor that changes the
    /// separator, drops the protocol field, or renames the empty-
    /// ports placeholder ("any") trips this test rather than
    /// silently breaking downstream parsers that read these strings.
    #[test]
    fn format_network_service_pins_host_ports_protocol_shape() {
        use crate::internal::ai::sandbox::{NetworkProtocol, NetworkService};

        // Single port + default protocol (None → tcp).
        assert_eq!(
            super::format_network_service(&NetworkService {
                host: "registry.npmjs.org".to_string(),
                ports: vec![443],
                protocol: None,
            }),
            "registry.npmjs.org:443:tcp",
        );

        // Multiple ports, comma-joined in declared order, default tcp.
        assert_eq!(
            super::format_network_service(&NetworkService {
                host: "github.com".to_string(),
                ports: vec![22, 443],
                protocol: Some(NetworkProtocol::Tcp),
            }),
            "github.com:22,443:tcp",
        );

        // Empty ports → "any" placeholder (entry permits every port).
        //
        // NOTE: `NetworkService::validate()` *rejects* empty `ports`
        // (it would implicitly grant the high-sensitivity SSH/RDP
        // ports), so a well-formed allowlist never contains this
        // shape. The "any" branch is still reachable in production
        // because `describe_network_access` formats the policy's raw
        // `allowlist_services()` *before* validation — when a
        // misconfigured policy carries an empty-ports service, the
        // allowlist proxy fails to construct (backend `none` +
        // warning) yet `network.allowlist` still echoes the raw entry
        // so the operator can see exactly what was misconfigured.
        // This case pins the formatter's "any" arm for that
        // diagnostic path.
        assert_eq!(
            super::format_network_service(&NetworkService {
                host: "*.internal.example".to_string(),
                ports: Vec::new(),
                protocol: Some(NetworkProtocol::Tcp),
            }),
            "*.internal.example:any:tcp",
        );

        // Explicit UDP overrides the tcp default.
        assert_eq!(
            super::format_network_service(&NetworkService {
                host: "dns.example".to_string(),
                ports: vec![53],
                protocol: Some(NetworkProtocol::Udp),
            }),
            "dns.example:53:udp",
        );

        // Wildcard host with explicit single port + default protocol.
        assert_eq!(
            super::format_network_service(&NetworkService {
                host: "*.pypi.org".to_string(),
                ports: vec![443],
                protocol: None,
            }),
            "*.pypi.org:443:tcp",
        );
    }

    #[test]
    fn sandbox_host_environment_warnings_detect_wsl_signals() {
        let warnings = sandbox_host_environment_warnings(&HostEnvironmentSignals {
            os: "linux",
            wsl_distro_name: Some("Ubuntu".to_string()),
            proc_version: None,
            docker_env_file_exists: false,
            container_env_file_exists: false,
            proc_1_cgroup: None,
        });

        assert!(
            warnings.iter().any(|warning| warning.contains("WSL")),
            "expected WSL warning, got: {warnings:?}"
        );
    }

    #[test]
    fn sandbox_host_environment_warnings_detect_container_signals() {
        let warnings = sandbox_host_environment_warnings(&HostEnvironmentSignals {
            os: "linux",
            wsl_distro_name: None,
            proc_version: None,
            docker_env_file_exists: false,
            container_env_file_exists: false,
            proc_1_cgroup: Some("0::/kubepods.slice/docker-123.scope".to_string()),
        });

        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("containerized environment")),
            "expected container warning, got: {warnings:?}"
        );
    }

    #[test]
    fn sandbox_host_environment_warnings_ignore_non_linux() {
        let warnings = sandbox_host_environment_warnings(&HostEnvironmentSignals {
            os: "macos",
            wsl_distro_name: Some("Ubuntu".to_string()),
            proc_version: Some("microsoft".to_string()),
            docker_env_file_exists: true,
            container_env_file_exists: true,
            proc_1_cgroup: Some("docker".to_string()),
        });

        assert!(
            warnings.is_empty(),
            "non-Linux hosts should not receive Linux sandbox warnings: {warnings:?}"
        );
    }
}
