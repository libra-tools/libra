//! Handler for the shell tool.
//!
//! Executes shell commands in the user's default shell with configurable
//! working directory and timeout. Output is capped to prevent memory issues.

use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use diffy::create_patch;

// SAFETY: The unwrap() and expect() calls in test code are acceptable as test
// failures are expected to panic on assertion failures.
use super::parse_arguments;
use crate::{
    internal::ai::{
        runtime::hardening::{CommandSafetySurface, SafetyDecision, SafetyDisposition},
        sandbox::{
            ShellCommandRequest,
            evidence::{
                FanoutSandboxEvidenceSink, InMemorySandboxEvidenceSink, SandboxEvidenceSink,
                TracingSandboxEvidenceSink,
            },
            run_shell_command_with_approval,
        },
        tools::{
            context::{ShellArgs, ToolInvocation, ToolKind, ToolOutput, ToolPayload},
            error::{ToolError, ToolResult},
            registry::ToolHandler,
            spec::ToolSpec,
            utils::{classify_ai_command_safety, is_within_working_dir, resolve_path},
        },
        workspace_snapshot::{
            WorkspaceSnapshot, changed_paths_since_baseline as changed_workspace_paths,
            is_cargo_cache_path, snapshot_workspace,
        },
    },
    utils::util::is_sub_path,
};

/// Handler for executing shell commands.
///
/// AI user story: let the agent run project-local verification commands
/// (builds, tests, formatters, scripts) and return bounded stdout/stderr plus
/// written-path metadata. Direct Git invocation is blocked so Libra-managed VCS
/// state changes go through audited Libra tools instead.
pub struct ShellHandler;

/// Maximum bytes captured per stream (stdout or stderr) before truncation.
const DEFAULT_MAX_OUTPUT_BYTES: usize = 100 * 1024; // 100 KiB
/// Maximum bytes captured from a Cargo.toml for policy diff metadata.
const SHELL_DIFF_MAX_FILE_BYTES: u64 = 256 * 1024;
/// Exit code emitted when a command is killed due to timeout (matches GNU timeout).
#[cfg(test)]
const TIMEOUT_EXIT_CODE: i32 = 124;

#[async_trait]
impl ToolHandler for ShellHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    /// Shell commands always mutate the environment.
    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        true
    }

    async fn handle(&self, invocation: ToolInvocation) -> ToolResult<ToolOutput> {
        let ToolInvocation {
            call_id,
            payload,
            working_dir,
            runtime_context,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(ToolError::IncompatiblePayload(
                    "shell handler only accepts Function payloads".to_string(),
                ));
            }
        };

        let args: ShellArgs = parse_arguments(&arguments)?;
        let safety_decision =
            classify_ai_command_safety(CommandSafetySurface::Shell, &args.command, &[]);
        enforce_shell_safety(&safety_decision)?;

        // Resolve and validate the execution working directory.
        let cwd = resolve_workdir(args.workdir.as_deref(), &working_dir)?;
        if let Some(path) =
            mutating_shell_target_outside_workspace(&args.command, &cwd, &working_dir)?
        {
            return Err(ToolError::ExecutionFailed(format!(
                "shell command targets a path outside the workspace and was rejected: {}",
                path.display()
            )));
        }

        let max_output_bytes = runtime_context
            .as_ref()
            .and_then(|ctx| ctx.max_output_bytes)
            .unwrap_or(DEFAULT_MAX_OUTPUT_BYTES);
        let sandbox_runtime = runtime_context
            .as_ref()
            .and_then(|ctx| ctx.sandbox_runtime.clone());
        let sandbox_evidence_capture = Arc::new(InMemorySandboxEvidenceSink::new());
        let sandbox_evidence_sink = shell_sandbox_evidence_sink(
            sandbox_runtime
                .as_ref()
                .and_then(|runtime| runtime.evidence_sink.clone()),
            sandbox_evidence_capture.clone(),
        );
        let approval = runtime_context
            .as_ref()
            .and_then(|ctx| ctx.approval.clone());
        let sandbox = runtime_context.as_ref().and_then(|ctx| {
            ctx.sandbox.clone().map(|mut sandbox| {
                sandbox.permissions = args.sandbox_permissions;
                sandbox
            })
        });
        let baseline_snapshot = snapshot_workspace(&working_dir).map_err(|err| {
            ToolError::ExecutionFailed(format!("failed to snapshot workspace: {err}"))
        })?;
        let baseline_manifest_contents =
            capture_cargo_manifest_contents(&working_dir, &baseline_snapshot)?;

        let command_for_error = args.command.clone();
        let output = match run_shell_command_with_approval(ShellCommandRequest {
            call_id,
            command: args.command,
            cwd: cwd.clone(),
            timeout_ms: args.timeout_ms,
            max_output_bytes,
            sandbox,
            sandbox_runtime,
            evidence_sink: Some(sandbox_evidence_sink),
            approval,
            justification: args.justification,
            safety_decision: Some(safety_decision.clone()),
        })
        .await
        {
            Ok(output) => output,
            Err(err) => {
                // Surface the command and cwd so the LLM has full context when
                // the sandbox refuses to execute, rather than just a bare
                // runtime error string.
                let rendered_error = format!(
                    "shell sandbox refused command (cwd={}): {}\ncommand: {}",
                    cwd.display(),
                    err,
                    command_for_error
                );
                let sandbox_evidence = sandbox_evidence_metadata(&sandbox_evidence_capture);
                if sandbox_evidence.is_empty() {
                    return Err(ToolError::ExecutionFailed(rendered_error));
                }
                let metadata = serde_json::json!({
                    "paths_written": [],
                    "diffs": [],
                    "safety": shell_safety_metadata(&safety_decision),
                    "sandbox_evidence": sandbox_evidence,
                });
                return Ok(ToolOutput::failure(rendered_error).with_metadata(metadata));
            }
        };
        let final_snapshot = snapshot_workspace(&working_dir).map_err(|err| {
            ToolError::ExecutionFailed(format!(
                "failed to inspect workspace changes after shell command: {err}"
            ))
        })?;
        let changed_paths = changed_workspace_paths(&baseline_snapshot, &final_snapshot);
        let metadata = serde_json::json!({
            "paths_written": changed_paths_to_strings(&changed_paths),
            "diffs": changed_cargo_manifest_diffs_since_baseline(
                &working_dir,
                &changed_paths,
                &baseline_snapshot,
                &final_snapshot,
                &baseline_manifest_contents,
            )?,
            "safety": shell_safety_metadata(&safety_decision),
            "sandbox_evidence": sandbox_evidence_metadata(&sandbox_evidence_capture),
        });

        let formatted = format_output(
            output.exit_code,
            &output.stdout,
            &output.stderr,
            output.timed_out,
            max_output_bytes,
        );
        let rendered = if output.exit_code == 0 {
            ToolOutput::success(formatted)
        } else {
            ToolOutput::failure(formatted)
        };
        Ok(rendered.with_metadata(metadata))
    }

    fn schema(&self) -> ToolSpec {
        ToolSpec::shell()
    }
}

fn shell_sandbox_evidence_sink(
    runtime_sink: Option<Arc<dyn SandboxEvidenceSink>>,
    capture: Arc<InMemorySandboxEvidenceSink>,
) -> Arc<dyn SandboxEvidenceSink> {
    let mut sinks: Vec<Arc<dyn SandboxEvidenceSink>> = Vec::new();
    if let Some(sink) = runtime_sink {
        sinks.push(sink);
    } else {
        sinks.push(Arc::new(TracingSandboxEvidenceSink));
    }
    sinks.push(capture);
    Arc::new(FanoutSandboxEvidenceSink::new(sinks))
}

fn sandbox_evidence_metadata(capture: &InMemorySandboxEvidenceSink) -> Vec<serde_json::Value> {
    capture
        .events()
        .iter()
        .map(|event| event.to_metadata_value())
        .collect()
}

// ── Output formatting ─────────────────────────────────────────────────────────

fn format_output(
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    timed_out: bool,
    max_output_bytes: usize,
) -> String {
    let mut parts: Vec<String> = Vec::new();

    if timed_out {
        parts.push("[Command timed out]".to_string());
    }
    parts.push(format!("Exit code: {exit_code}"));

    if !stdout.is_empty() {
        parts.push(String::new()); // blank separator line
        parts.push(stdout.to_string());
        if stdout.contains("[stdout truncated]") {
            parts.push(format!(
                "[truncated: stdout exceeded {max_output_bytes} bytes]"
            ));
        }
    }
    if !stderr.is_empty() {
        parts.push("[stderr]".to_string());
        parts.push(stderr.to_string());
        if stderr.contains("[stderr truncated]") {
            parts.push(format!(
                "[truncated: stderr exceeded {max_output_bytes} bytes]"
            ));
        }
    }

    parts.join("\n")
}

fn resolve_workdir(requested_workdir: Option<&str>, working_dir: &Path) -> ToolResult<PathBuf> {
    let Some(workdir) = requested_workdir else {
        return Ok(working_dir.to_path_buf());
    };

    let requested = Path::new(workdir);
    let resolved = resolve_path(requested, working_dir)?;

    let requested_canon = std::fs::canonicalize(&resolved).map_err(|e| {
        ToolError::ExecutionFailed(format!(
            "failed to canonicalize workdir '{}': {e}",
            requested.display()
        ))
    })?;
    let working_dir_canon = std::fs::canonicalize(working_dir).map_err(|e| {
        ToolError::ExecutionFailed(format!(
            "failed to canonicalize working_dir '{}': {e}",
            working_dir.display()
        ))
    })?;

    if !is_sub_path(&requested_canon, &working_dir_canon) {
        return Err(ToolError::PathOutsideWorkingDir(resolved));
    }

    Ok(requested_canon)
}

fn enforce_shell_safety(decision: &SafetyDecision) -> ToolResult<()> {
    if !matches!(decision.disposition, SafetyDisposition::Deny) {
        return Ok(());
    }

    Err(ToolError::ExecutionFailed(shell_safety_denial_message(
        decision,
    )))
}

fn shell_safety_denial_message(decision: &SafetyDecision) -> String {
    if decision.rule_name == "shell.direct_git_forbidden" {
        return format!(
            "git is not allowed for Libra-managed agent execution; use the run_libra_vcs tool or a libra command instead (safety rule: {}; blast radius: {}; reason: {})",
            decision.rule_name, decision.blast_radius, decision.reason
        );
    }

    format!(
        "shell safety denied command (rule: {}; blast radius: {}; reason: {})",
        decision.rule_name, decision.blast_radius, decision.reason
    )
}

fn shell_safety_metadata(decision: &SafetyDecision) -> serde_json::Value {
    serde_json::json!({
        "disposition": decision.disposition,
        "rule_name": decision.rule_name,
        "blast_radius": decision.blast_radius,
    })
}

fn mutating_shell_target_outside_workspace(
    command: &str,
    cwd: &Path,
    working_dir: &Path,
) -> ToolResult<Option<PathBuf>> {
    let Some(parts) = shlex::split(command) else {
        return Ok(None);
    };

    mutating_words_target_outside_workspace(&parts, cwd, working_dir)
}

fn mutating_words_target_outside_workspace(
    parts: &[String],
    cwd: &Path,
    working_dir: &Path,
) -> ToolResult<Option<PathBuf>> {
    let Some((cmd, args)) = normalized_shell_words(parts) else {
        return Ok(None);
    };

    if is_shell_interpreter(&cmd)
        && let Some(script) = shell_c_script(args)
    {
        return mutating_shell_target_outside_workspace(script, cwd, working_dir);
    }

    let path_args = mutating_shell_path_args(&cmd, args);
    for path_arg in path_args {
        let path = Path::new(path_arg);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        if !is_within_working_dir(&resolved, working_dir)? {
            return Ok(Some(resolved));
        }
    }

    Ok(None)
}

fn normalized_shell_words(parts: &[String]) -> Option<(String, &[String])> {
    let mut idx = 0;
    while let Some(word) = parts.get(idx).map(String::as_str) {
        match executable_name(word).as_deref() {
            Some("command") | Some("sudo") => idx += 1,
            Some("env") => {
                idx += 1;
                idx += skip_env_prefix(&parts[idx..]);
            }
            _ => break,
        }
    }

    let cmd = executable_name(parts.get(idx).map(String::as_str)?)?;
    Some((cmd, &parts[idx + 1..]))
}

fn skip_env_prefix(args: &[String]) -> usize {
    let mut idx = 0;
    while let Some(arg) = args.get(idx).map(String::as_str) {
        if arg.contains('=') && !arg.starts_with('-') {
            idx += 1;
            continue;
        }
        if arg.starts_with("--unset=") || arg.starts_with("--chdir=") {
            idx += 1;
            continue;
        }
        if matches!(arg, "-i" | "--ignore-environment" | "-0" | "--null") {
            idx += 1;
            continue;
        }
        if matches!(arg, "-u" | "--unset" | "-C" | "--chdir") {
            idx += 1;
            if idx < args.len() {
                idx += 1;
            }
            continue;
        }
        break;
    }
    idx
}

fn mutating_shell_path_args<'a>(cmd: &str, args: &'a [String]) -> Vec<&'a str> {
    match cmd {
        "touch" | "mkdir" | "rm" | "rmdir" | "cp" | "mv" => shell_non_option_args(args),
        "chmod" | "chown" => {
            let mut paths = shell_non_option_args(args);
            if !paths.is_empty() {
                paths.remove(0);
            }
            paths
        }
        _ => Vec::new(),
    }
}

fn shell_non_option_args(args: &[String]) -> Vec<&str> {
    let mut after_double_dash = false;
    args.iter()
        .filter_map(|arg| {
            if after_double_dash {
                return Some(arg.as_str());
            }
            if arg == "--" {
                after_double_dash = true;
                return None;
            }
            if arg.starts_with('-') {
                None
            } else {
                Some(arg.as_str())
            }
        })
        .collect()
}

fn is_shell_interpreter(cmd: &str) -> bool {
    matches!(cmd, "bash" | "dash" | "sh" | "zsh")
}

fn shell_c_script(args: &[String]) -> Option<&str> {
    args.windows(2).find_map(|pair| {
        if pair[0] == "-c" {
            Some(pair[1].as_str())
        } else {
            None
        }
    })
}

fn executable_name(command: &str) -> Option<String> {
    Path::new(command)
        .file_name()
        .and_then(OsStr::to_str)
        .map(|name| name.trim_end_matches(".exe").to_ascii_lowercase())
}

fn changed_paths_to_strings(changed_paths: &[PathBuf]) -> Vec<String> {
    changed_paths
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect()
}

fn changed_cargo_manifest_diffs_since_baseline(
    root: &Path,
    changed_paths: &[PathBuf],
    baseline: &WorkspaceSnapshot,
    current: &WorkspaceSnapshot,
    baseline_manifest_contents: &BTreeMap<PathBuf, String>,
) -> ToolResult<Vec<serde_json::Value>> {
    changed_paths
        .iter()
        .filter_map(|path| {
            if !is_cargo_manifest_path(path) {
                return None;
            }
            let change_type = match (
                baseline.entries.contains_key(path),
                current.entries.contains_key(path),
            ) {
                (false, true) => "add",
                (true, false) => "delete",
                _ => "update",
            };
            Some((path, change_type))
        })
        .map(|(path, change_type)| {
            let old_content = if baseline.entries.contains_key(path) {
                match baseline_manifest_contents.get(path) {
                    Some(content) => content.clone(),
                    None => return Ok(None),
                }
            } else {
                String::new()
            };
            let new_content = if current.entries.contains_key(path) {
                match read_capped_utf8_file(&root.join(path))? {
                    Some(content) => content,
                    None => return Ok(None),
                }
            } else {
                String::new()
            };
            Ok(Some(serde_json::json!({
                "path": path.to_string_lossy().to_string(),
                "diff": create_patch(&old_content, &new_content).to_string(),
                "type": change_type,
            })))
        })
        .filter_map(Result::transpose)
        .collect()
}

fn capture_cargo_manifest_contents(
    root: &Path,
    snapshot: &WorkspaceSnapshot,
) -> ToolResult<BTreeMap<PathBuf, String>> {
    let mut contents = BTreeMap::new();
    for path in snapshot
        .entries
        .keys()
        .filter(|path| is_cargo_manifest_path(path))
    {
        if let Some(content) = read_capped_utf8_file(&root.join(path))? {
            contents.insert(path.clone(), content);
        }
    }
    Ok(contents)
}

fn read_capped_utf8_file(path: &Path) -> ToolResult<Option<String>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(ToolError::ExecutionFailed(format!(
                "failed to inspect '{}': {err}",
                path.display()
            )));
        }
    };
    if !metadata.is_file() || metadata.len() > SHELL_DIFF_MAX_FILE_BYTES {
        return Ok(None);
    }
    let bytes = fs::read(path).map_err(|err| {
        ToolError::ExecutionFailed(format!("failed to read '{}': {err}", path.display()))
    })?;
    match String::from_utf8(bytes) {
        Ok(content) => Ok(Some(content)),
        Err(_) => Ok(None),
    }
}

fn is_cargo_manifest_path(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == "Cargo.toml") && !is_cargo_cache_path(path)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use serial_test::serial;
    use tempfile::TempDir;

    use super::*;
    use crate::internal::ai::{
        sandbox::{
            ApprovalCachePolicy, ApprovalStore, AskForApproval, ExecApprovalRequest, NetworkAccess,
            ReviewDecision, SandboxPermissions, SandboxPolicy, ToolApprovalContext,
            ToolRuntimeContext, ToolSandboxContext,
        },
        tools::context::ToolPayload,
    };

    fn make_invocation(args: serde_json::Value, working_dir: std::path::PathBuf) -> ToolInvocation {
        ToolInvocation::new(
            "call-1",
            "shell",
            ToolPayload::Function {
                arguments: args.to_string(),
            },
            working_dir,
        )
    }

    async fn handle_invocation_with_tempdir(
        inv: ToolInvocation,
        keepalive: &TempDir,
    ) -> ToolResult<ToolOutput> {
        let result = ShellHandler.handle(inv).await;
        let _ = keepalive.path();
        result
    }

    fn runtime_with_approval(
        policy: AskForApproval,
    ) -> (
        ToolRuntimeContext,
        tokio::sync::mpsc::UnboundedReceiver<ExecApprovalRequest>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (
            ToolRuntimeContext {
                sandbox: Some(ToolSandboxContext {
                    policy: SandboxPolicy::WorkspaceWrite {
                        writable_roots: Vec::new(),
                        network_access: NetworkAccess::Denied,
                        exclude_tmpdir_env_var: false,
                        exclude_slash_tmp: false,
                        allow_metadata_writes: false,
                    },
                    permissions: SandboxPermissions::UseDefault,
                }),
                sandbox_runtime: None,
                approval: Some(ToolApprovalContext {
                    policy,
                    request_tx: tx,
                    store: Arc::new(tokio::sync::Mutex::new(ApprovalStore::default())),
                    scope_key_prefix: None,
                    approval_ttl: std::time::Duration::from_secs(300),
                    cache_policy: ApprovalCachePolicy::default(),
                }),
                file_history: None,
                max_output_bytes: Some(DEFAULT_MAX_OUTPUT_BYTES),
            },
            rx,
        )
    }

    #[tokio::test]
    async fn rejects_git_version_control_commands() {
        let temp = TempDir::new().unwrap();
        let inv = make_invocation(
            serde_json::json!({ "command": "git status" }),
            temp.path().to_path_buf(),
        );

        let error = ShellHandler
            .handle(inv)
            .await
            .expect_err("git shell command should be rejected");

        assert!(error.to_string().contains("git is not allowed"));
    }

    #[tokio::test]
    async fn rejects_destructive_shell_commands_before_spawn() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("artifact"), "keep").unwrap();
        let inv = make_invocation(
            serde_json::json!({ "command": "rm -rf target" }),
            temp.path().to_path_buf(),
        );

        let error = ShellHandler
            .handle(inv)
            .await
            .expect_err("destructive shell command should be rejected");

        let rendered = error.to_string();
        assert!(rendered.contains("shell safety denied"), "{rendered}");
        assert!(rendered.contains("shell.destructive_command"), "{rendered}");
        assert!(
            target.exists(),
            "destructive command should be rejected before spawn"
        );
    }

    #[tokio::test]
    async fn rejects_wrapped_destructive_shell_commands_before_spawn() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("wrapped-target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("artifact"), "keep").unwrap();
        let inv = make_invocation(
            serde_json::json!({ "command": "env rm -rf wrapped-target" }),
            temp.path().to_path_buf(),
        );

        let error = ShellHandler
            .handle(inv)
            .await
            .expect_err("wrapped destructive shell command should be rejected");

        assert!(
            error.to_string().contains("shell.destructive_command"),
            "{error}"
        );
        assert!(
            target.exists(),
            "wrapped destructive command should be rejected before spawn"
        );
    }

    #[tokio::test]
    async fn needs_human_shell_safety_uses_approval_prompt() {
        let temp = TempDir::new().unwrap();
        let (runtime_context, mut approval_rx) = runtime_with_approval(AskForApproval::OnRequest);
        let inv = make_invocation(
            serde_json::json!({ "command": "touch generated.txt" }),
            temp.path().to_path_buf(),
        )
        .with_runtime_context(runtime_context);

        let task = tokio::spawn(async move { ShellHandler.handle(inv).await });
        let approval = approval_rx
            .recv()
            .await
            .expect("needs-human shell command should request approval");

        assert_eq!(approval.command, "touch generated.txt");
        let reason = approval.reason.as_deref().unwrap_or_default();
        assert!(
            reason.contains("shell.workspace_mutation_or_execution"),
            "{reason}"
        );
        approval.response_tx.send(ReviewDecision::Denied).unwrap();

        let error = task.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("rejected by user"));
        assert!(!temp.path().join("generated.txt").exists());
    }

    #[tokio::test]
    async fn mutating_shell_command_cannot_target_outside_workspace() {
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let outside_file = outside.path().join("created-by-shell");
        let inv = make_invocation(
            serde_json::json!({ "command": format!("touch {}", outside_file.display()) }),
            workspace.path().to_path_buf(),
        );

        let error = ShellHandler
            .handle(inv)
            .await
            .expect_err("outside-workspace target should be rejected");

        assert!(
            error.to_string().contains("outside the workspace"),
            "{error}"
        );
        assert!(!outside_file.exists());
    }

    // ── Basic execution ───────────────────────────────────────────────────────

    #[tokio::test]
    #[serial(rpc_path_env)]
    async fn test_shell_echo() {
        let temp = TempDir::new().unwrap();
        let inv = make_invocation(
            serde_json::json!({ "command": "echo hello" }),
            temp.path().to_path_buf(),
        );
        let result = handle_invocation_with_tempdir(inv, &temp).await.unwrap();
        assert!(result.is_success());
        assert!(result.as_text().unwrap().contains("hello"));
    }

    #[tokio::test]
    #[serial(rpc_path_env)]
    async fn test_shell_exit_code_zero() {
        let temp = TempDir::new().unwrap();
        let inv = make_invocation(
            serde_json::json!({ "command": "true" }),
            temp.path().to_path_buf(),
        );
        let result = handle_invocation_with_tempdir(inv, &temp).await.unwrap();
        assert!(result.is_success());
        assert!(result.as_text().unwrap().contains("Exit code: 0"));
    }

    #[tokio::test]
    #[serial(rpc_path_env)]
    async fn test_shell_exit_code_nonzero() {
        let temp = TempDir::new().unwrap();
        let inv = make_invocation(
            serde_json::json!({ "command": "exit 42" }),
            temp.path().to_path_buf(),
        );
        let result = handle_invocation_with_tempdir(inv, &temp).await.unwrap();
        assert!(!result.is_success());
        assert!(result.as_text().unwrap().contains("Exit code: 42"));
    }

    #[tokio::test]
    #[serial(rpc_path_env)]
    async fn test_shell_multiline_output() {
        let temp = TempDir::new().unwrap();
        let inv = make_invocation(
            serde_json::json!({ "command": "printf 'line1\\nline2\\nline3\\n'" }),
            temp.path().to_path_buf(),
        );
        let result = handle_invocation_with_tempdir(inv, &temp).await.unwrap();
        let text = result.as_text().unwrap();
        assert!(text.contains("line1"), "{text}");
        assert!(text.contains("line2"), "{text}");
        assert!(text.contains("line3"), "{text}");
    }

    // ── Stderr ────────────────────────────────────────────────────────────────

    #[tokio::test]
    #[serial(rpc_path_env)]
    async fn test_shell_stderr_captured() {
        let temp = TempDir::new().unwrap();
        let inv = make_invocation(
            serde_json::json!({ "command": "echo error_msg >&2" }),
            temp.path().to_path_buf(),
        );
        let result = handle_invocation_with_tempdir(inv, &temp).await.unwrap();
        let text = result.as_text().unwrap();
        assert!(text.contains("error_msg"), "stderr not captured:\n{text}");
    }

    #[tokio::test]
    #[serial(rpc_path_env)]
    async fn test_shell_stderr_section_label() {
        let temp = TempDir::new().unwrap();
        let inv = make_invocation(
            serde_json::json!({ "command": "echo out; echo err >&2" }),
            temp.path().to_path_buf(),
        );
        let result = handle_invocation_with_tempdir(inv, &temp).await.unwrap();
        let text = result.as_text().unwrap();
        assert!(
            text.contains("[stderr]"),
            "expected [stderr] label:\n{text}"
        );
        assert!(text.contains("out"), "{text}");
        assert!(text.contains("err"), "{text}");
    }

    // ── Working directory ─────────────────────────────────────────────────────

    #[tokio::test]
    #[serial(rpc_path_env)]
    async fn test_shell_workdir_default() {
        let temp = TempDir::new().unwrap();
        let working_dir = temp.path().to_path_buf();
        let inv = make_invocation(serde_json::json!({ "command": "pwd" }), working_dir.clone());
        let result = handle_invocation_with_tempdir(inv, &temp).await.unwrap();
        let text = result.as_text().unwrap();
        // Compare the last path component to avoid macOS /tmp → /private/tmp symlink issues.
        let dir_name = working_dir.file_name().unwrap().to_str().unwrap();
        assert!(
            text.contains(dir_name),
            "expected dir name in output:\n{text}"
        );
    }

    #[tokio::test]
    #[serial(rpc_path_env)]
    async fn test_shell_workdir_override() {
        let outer = TempDir::new().unwrap();
        let inner_path = outer.path().join("inner_subdir");
        std::fs::create_dir(&inner_path).unwrap();

        let inv = make_invocation(
            serde_json::json!({
                "command": "pwd",
                "workdir": inner_path.to_str().unwrap()
            }),
            outer.path().to_path_buf(),
        );
        let result = handle_invocation_with_tempdir(inv, &outer).await.unwrap();
        let text = result.as_text().unwrap();
        assert!(
            text.contains("inner_subdir"),
            "expected inner_subdir in output:\n{text}"
        );
    }

    #[tokio::test]
    async fn test_shell_workdir_outside_sandbox_fails() {
        let sandbox = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();

        let inv = make_invocation(
            serde_json::json!({
                "command": "pwd",
                "workdir": outside.path().to_str().unwrap()
            }),
            sandbox.path().to_path_buf(),
        );
        let result = ShellHandler.handle(inv).await;
        assert!(
            matches!(result, Err(ToolError::PathOutsideWorkingDir(_))),
            "expected PathOutsideWorkingDir, got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_shell_workdir_symlink_escape_fails() {
        use std::os::unix::fs::symlink;

        let sandbox = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let link_path = sandbox.path().join("escape");
        symlink(outside.path(), &link_path).unwrap();

        let inv = make_invocation(
            serde_json::json!({
                "command": "pwd",
                "workdir": link_path.to_str().unwrap()
            }),
            sandbox.path().to_path_buf(),
        );
        let result = ShellHandler.handle(inv).await;
        assert!(
            matches!(result, Err(ToolError::PathOutsideWorkingDir(_))),
            "expected PathOutsideWorkingDir, got: {result:?}"
        );
    }

    #[tokio::test]
    #[serial(rpc_path_env)]
    async fn test_shell_workdir_relative_path_is_resolved_inside_sandbox() {
        let temp = TempDir::new().unwrap();
        let inner_path = temp.path().join("relative").join("path");
        std::fs::create_dir_all(&inner_path).unwrap();

        let inv = make_invocation(
            serde_json::json!({
                "command": "pwd",
                "workdir": "relative/path"
            }),
            temp.path().to_path_buf(),
        );
        let result = handle_invocation_with_tempdir(inv, &temp).await.unwrap();
        let text = result.as_text().unwrap();
        assert!(
            text.contains("relative/path") || text.contains("relative\\path"),
            "expected resolved relative/path in output:\n{text}"
        );
    }

    #[tokio::test]
    #[serial(rpc_path_env)]
    async fn test_shell_workdir_dot_uses_sandbox_root() {
        let temp = TempDir::new().unwrap();
        let inv = make_invocation(
            serde_json::json!({
                "command": "pwd",
                "workdir": "."
            }),
            temp.path().to_path_buf(),
        );
        let result = handle_invocation_with_tempdir(inv, &temp).await.unwrap();
        let text = result.as_text().unwrap();
        let dir_name = temp.path().file_name().unwrap().to_str().unwrap();
        assert!(
            text.contains(dir_name),
            "expected sandbox root in output:\n{text}"
        );
    }

    // ── Timeout ───────────────────────────────────────────────────────────────

    #[tokio::test]
    #[serial(rpc_path_env)]
    async fn test_shell_timeout() {
        let temp = TempDir::new().unwrap();
        let inv = make_invocation(
            serde_json::json!({ "command": "sleep 60", "timeout_ms": 200 }),
            temp.path().to_path_buf(),
        );
        let result = handle_invocation_with_tempdir(inv, &temp).await.unwrap();
        let text = result.as_text().unwrap();
        assert!(
            text.contains("[Command timed out]"),
            "expected timeout notice:\n{text}"
        );
        assert!(
            text.contains(&format!("Exit code: {TIMEOUT_EXIT_CODE}")),
            "expected exit code {TIMEOUT_EXIT_CODE}:\n{text}"
        );
        assert!(!result.is_success());
    }

    #[tokio::test]
    #[serial(rpc_path_env)]
    async fn test_shell_background_child_does_not_hang() {
        let temp = TempDir::new().unwrap();
        let inv = make_invocation(
            serde_json::json!({ "command": "sleep 5 & echo done", "timeout_ms": 5000 }),
            temp.path().to_path_buf(),
        );

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            handle_invocation_with_tempdir(inv, &temp),
        )
        .await
        .expect("shell handler should not hang")
        .unwrap();
        let text = result.as_text().unwrap();
        assert!(text.contains("Exit code: 0"), "{text}");
        assert!(text.contains("done"), "{text}");
    }

    // ── Output truncation ─────────────────────────────────────────────────────

    #[tokio::test]
    #[serial(rpc_path_env)]
    async fn test_shell_large_output_truncated() {
        let temp = TempDir::new().unwrap();
        // seq 1 200000 produces ~1.4 MB; MAX_OUTPUT_BYTES = 100 KiB → should truncate.
        let inv = make_invocation(
            serde_json::json!({ "command": "seq 1 200000" }),
            temp.path().to_path_buf(),
        );
        let result = handle_invocation_with_tempdir(inv, &temp).await.unwrap();
        let text = result.as_text().unwrap();
        assert!(
            text.contains("truncated"),
            "expected truncation notice:\n{text}"
        );
    }

    #[tokio::test]
    #[serial(rpc_path_env)]
    async fn test_shell_metadata_tracks_written_paths() {
        let temp = TempDir::new().unwrap();
        let outside_repo = TempDir::new().unwrap();
        let _cwd = crate::utils::test::ChangeDirGuard::new(outside_repo.path());
        let inv = make_invocation(
            serde_json::json!({ "command": "printf 'hello\\n' > touched.txt" }),
            temp.path().to_path_buf(),
        );
        let result = handle_invocation_with_tempdir(inv, &temp).await.unwrap();

        let metadata = result
            .metadata()
            .expect("shell results should include metadata");
        assert_eq!(
            metadata["paths_written"],
            serde_json::json!(["touched.txt"])
        );
    }

    #[tokio::test]
    async fn test_shell_metadata_captures_sandbox_rejection_evidence() {
        let temp = TempDir::new().unwrap();
        let inv = make_invocation(
            serde_json::json!({ "command": "true" }),
            temp.path().to_path_buf(),
        )
        .with_runtime_context(ToolRuntimeContext {
            sandbox: Some(ToolSandboxContext {
                policy: SandboxPolicy::WorkspaceWrite {
                    writable_roots: vec![PathBuf::from("/")],
                    network_access: NetworkAccess::Denied,
                    exclude_tmpdir_env_var: false,
                    exclude_slash_tmp: false,
                    allow_metadata_writes: false,
                },
                permissions: SandboxPermissions::UseDefault,
            }),
            sandbox_runtime: None,
            approval: None,
            file_history: None,
            max_output_bytes: Some(DEFAULT_MAX_OUTPUT_BYTES),
        });

        let result = handle_invocation_with_tempdir(inv, &temp).await.unwrap();

        assert!(!result.is_success());
        let metadata = result
            .metadata()
            .expect("sandbox failures should keep evidence metadata");
        let events = metadata["sandbox_evidence"]
            .as_array()
            .expect("sandbox evidence should be an array");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "writable_root_rejected");
        assert_eq!(events[0]["root"], "/");
    }

    #[tokio::test]
    #[serial(rpc_path_env)]
    async fn test_shell_metadata_includes_text_file_diffs() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("Cargo.toml"), "[dependencies]\n").unwrap();
        let outside_repo = TempDir::new().unwrap();
        let _cwd = crate::utils::test::ChangeDirGuard::new(outside_repo.path());
        let inv = make_invocation(
            serde_json::json!({ "command": "printf 'serde = \"1\"\\n' >> Cargo.toml" }),
            temp.path().to_path_buf(),
        );
        let result = handle_invocation_with_tempdir(inv, &temp).await.unwrap();

        let metadata = result
            .metadata()
            .expect("shell results should include metadata");
        assert_eq!(metadata["paths_written"], serde_json::json!(["Cargo.toml"]));
        let diffs = metadata["diffs"]
            .as_array()
            .expect("shell metadata should include diffs");
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0]["path"], "Cargo.toml");
        assert_eq!(diffs[0]["type"], "update");
        assert!(
            diffs[0]["diff"]
                .as_str()
                .is_some_and(|diff| diff.contains("+serde = \"1\"")),
            "{diffs:?}"
        );
    }

    #[tokio::test]
    #[serial(rpc_path_env)]
    async fn test_shell_metadata_ignores_cargo_home_registry_manifest_diffs() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join("cargo-home/registry/src/index/dep-1.0.0"))
            .unwrap();
        let manifest = temp
            .path()
            .join("cargo-home/registry/src/index/dep-1.0.0/Cargo.toml");
        std::fs::write(&manifest, "[dependencies]\n").unwrap();
        let outside_repo = TempDir::new().unwrap();
        let _cwd = crate::utils::test::ChangeDirGuard::new(outside_repo.path());
        let inv = make_invocation(
            serde_json::json!({
                "command": "printf 'clap = \"4\"\\n' >> cargo-home/registry/src/index/dep-1.0.0/Cargo.toml"
            }),
            temp.path().to_path_buf(),
        );
        let result = handle_invocation_with_tempdir(inv, &temp).await.unwrap();

        let metadata = result
            .metadata()
            .expect("shell results should include metadata");
        assert_eq!(metadata["diffs"], serde_json::json!([]));
    }

    #[tokio::test]
    #[serial(rpc_path_env)]
    async fn test_shell_metadata_omits_non_manifest_diffs() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/lib.rs"), "pub fn base() {}\n").unwrap();
        let outside_repo = TempDir::new().unwrap();
        let _cwd = crate::utils::test::ChangeDirGuard::new(outside_repo.path());
        let inv = make_invocation(
            serde_json::json!({ "command": "printf 'pub fn changed() {}\\n' > src/lib.rs" }),
            temp.path().to_path_buf(),
        );
        let result = handle_invocation_with_tempdir(inv, &temp).await.unwrap();

        let metadata = result
            .metadata()
            .expect("shell results should include metadata");
        assert_eq!(metadata["paths_written"], serde_json::json!(["src/lib.rs"]));
        assert_eq!(metadata["diffs"], serde_json::json!([]));
    }

    // ── Handler metadata ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_shell_kind_is_function() {
        assert_eq!(ShellHandler.kind(), ToolKind::Function);
    }

    #[tokio::test]
    async fn test_shell_schema() {
        let schema = ShellHandler.schema();
        assert_eq!(schema.function.name, "shell");
        assert!(schema.function.description.len() > 10);
        let json = schema.to_json();
        assert_eq!(json["type"], "function");
        assert_eq!(json["function"]["name"], "shell");
        // "command" must be a required parameter.
        let required = json["function"]["parameters"]["required"]
            .as_array()
            .unwrap();
        assert!(
            required.iter().any(|v| v == "command"),
            "command should be required"
        );

        let timeout_description =
            json["function"]["parameters"]["properties"]["timeout_ms"]["description"]
                .as_str()
                .unwrap();
        assert!(
            timeout_description.contains("default: 60000"),
            "timeout default should match shell runtime default: {timeout_description}"
        );
    }

    #[tokio::test]
    async fn test_shell_is_mutating() {
        let temp = TempDir::new().unwrap();
        let inv = make_invocation(
            serde_json::json!({ "command": "echo hi" }),
            temp.path().to_path_buf(),
        );
        assert!(ShellHandler.is_mutating(&inv).await);
    }

    #[tokio::test]
    async fn test_shell_incompatible_payload() {
        let temp = TempDir::new().unwrap();
        let inv = ToolInvocation::new(
            "call-1",
            "shell",
            ToolPayload::Custom {
                input: "test".to_string(),
            },
            temp.path().to_path_buf(),
        );
        let result = ShellHandler.handle(inv).await;
        assert!(
            matches!(result, Err(ToolError::IncompatiblePayload(_))),
            "expected IncompatiblePayload"
        );
    }

    // ── Unit tests for internal helpers ───────────────────────────────────────

    #[test]
    fn test_format_output_success_stdout_only() {
        let text = format_output(0, "hello world\n", "", false, DEFAULT_MAX_OUTPUT_BYTES);
        assert!(text.contains("Exit code: 0"));
        assert!(text.contains("hello world"));
        assert!(!text.contains("[stderr]"));
        assert!(!text.contains("[Command timed out]"));
    }

    #[test]
    fn test_format_output_failure_with_stderr() {
        let text = format_output(1, "", "error occurred\n", false, DEFAULT_MAX_OUTPUT_BYTES);
        assert!(text.contains("Exit code: 1"));
        assert!(text.contains("[stderr]"));
        assert!(text.contains("error occurred"));
    }

    #[test]
    fn test_format_output_timed_out() {
        let text = format_output(TIMEOUT_EXIT_CODE, "", "", true, DEFAULT_MAX_OUTPUT_BYTES);
        assert!(text.contains("[Command timed out]"));
        assert!(text.contains(&format!("Exit code: {TIMEOUT_EXIT_CODE}")));
    }

    #[test]
    fn test_format_output_both_streams() {
        let text = format_output(
            0,
            "stdout content\n",
            "stderr content\n",
            false,
            DEFAULT_MAX_OUTPUT_BYTES,
        );
        assert!(text.contains("stdout content"));
        assert!(text.contains("[stderr]"));
        assert!(text.contains("stderr content"));
    }

    #[test]
    fn test_format_output_adds_explicit_truncation_markers() {
        let text = format_output(
            0,
            "partial stdout\n[stdout truncated]",
            "partial stderr\n[stderr truncated]",
            false,
            123,
        );

        assert!(text.contains("[truncated: stdout exceeded 123 bytes]"));
        assert!(text.contains("[truncated: stderr exceeded 123 bytes]"));
    }
}
