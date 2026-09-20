//! Hook runner: spawns hook commands, feeds them JSON, and interprets exit codes.
//!
//! The runner is the bridge between Libra's internal lifecycle events and the user's
//! external scripts. It spawns each matching hook in a `sh -c` subshell, writes the
//! [`HookInput`] JSON payload to stdin, and translates the resulting exit code into
//! a [`HookAction`]:
//! - `0` - allow.
//! - `129` - block (PreToolUse only); the rejection reason is read from stdout,
//!   falling back to `"Blocked by hook"`.
//! - Any other exit code - error, logged at `warn` level but never blocks.
//!
//! Hooks are isolated per invocation: the runner uses `kill_on_drop(true)` so a
//! hook that overruns its `timeout_ms` is terminated together with the pending
//! agent step.

use std::{io::ErrorKind, path::Path, time::Duration};

use tokio::{io::AsyncWriteExt, process::Command};

use super::{
    config::{HookConfig, HookDefinition},
    event::{HookAction, HookEvent, HookInput, HookOutput},
};

/// Stateful hook executor parameterised by a loaded [`HookConfig`].
///
/// Held by the agent runtime for the duration of a session. The runner is
/// inexpensive to clone-via-`HookRunner::load` and intentionally does not perform
/// any work beyond reading config.
#[derive(Debug)]
pub struct HookRunner {
    config: HookConfig,
    working_dir: std::path::PathBuf,
}

impl HookRunner {
    /// Construct a runner bound to a pre-loaded config.
    ///
    /// Useful for tests that want to inject a synthetic config without touching the
    /// filesystem.
    pub fn new(config: HookConfig, working_dir: std::path::PathBuf) -> Self {
        Self {
            config,
            working_dir,
        }
    }

    /// Load `hooks.json` from the project + user tiers and build a runner.
    ///
    /// Missing config files are tolerated (empty runner). Unreadable or
    /// malformed repository/overlay JSON fails closed.
    pub fn load(working_dir: &Path) -> Result<Self, String> {
        let config = super::config::load_hook_config(working_dir)?;
        Ok(Self::new(config, working_dir.to_path_buf()))
    }

    /// Rebind to `working_dir` without reloading `hooks.json`.
    ///
    /// Isolated task worktrees keep the session's RequestScope-resolved hook
    /// definitions but must execute them against the workspace being modified.
    pub fn with_working_dir(&self, working_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            config: self.config.clone(),
            working_dir: working_dir.into(),
        }
    }

    /// Quick existence check used by the agent runtime to skip hook plumbing
    /// entirely when no hooks are configured.
    pub fn has_hooks(&self) -> bool {
        self.config.hooks.iter().any(|h| h.enabled)
    }

    /// Run every enabled `PreToolUse` hook that matches `tool_name`.
    ///
    /// Functional scope: hooks are evaluated in the order they appear in the
    /// merged config; the first hook to return [`HookResult::Block`] aborts the
    /// chain and the runner returns the corresponding [`HookAction::Block`].
    ///
    /// Boundary conditions:
    /// - Hook errors and timeouts are logged but do **not** block. This is a
    ///   conscious safety/availability trade-off — a misconfigured hook should
    ///   not strand the user.
    /// - When no hooks match, `HookAction::Allow` is returned without spawning a
    ///   subshell.
    ///
    /// See: `tests::test_pre_tool_use_block`,
    /// `tests::test_pre_tool_use_no_matching_hooks`.
    pub async fn run_pre_tool_use(
        &self,
        tool_name: &str,
        tool_input: serde_json::Value,
    ) -> HookAction {
        let matching: Vec<&HookDefinition> = self
            .config
            .hooks
            .iter()
            .filter(|h| h.enabled && h.event == HookEvent::PreToolUse && h.matches_tool(tool_name))
            .collect();

        if matching.is_empty() {
            return HookAction::Allow;
        }

        let input =
            HookInput::pre_tool_use(tool_name, tool_input, &self.working_dir.to_string_lossy());

        for hook in matching {
            match self.execute_hook(hook, &input).await {
                HookResult::Allow => continue,
                HookResult::Block(reason) => return HookAction::Block(reason),
                HookResult::Error(err) => {
                    tracing::warn!("PreToolUse hook '{}' failed: {}", hook.description, err);
                    // Errors in hooks don't block by default
                    continue;
                }
            }
        }

        HookAction::Allow
    }

    /// Fire matching `PostToolUse` hooks for observability/automation.
    ///
    /// Functional scope: receives both the tool input and output JSON so observer
    /// hooks (formatters, log shippers, etc.) can see the full request/response
    /// pair.
    ///
    /// Boundary conditions: post-hooks cannot block — only errors are logged.
    pub async fn run_post_tool_use(
        &self,
        tool_name: &str,
        tool_input: serde_json::Value,
        tool_output: serde_json::Value,
    ) {
        let matching: Vec<&HookDefinition> = self
            .config
            .hooks
            .iter()
            .filter(|h| h.enabled && h.event == HookEvent::PostToolUse && h.matches_tool(tool_name))
            .collect();

        if matching.is_empty() {
            return;
        }

        let input = HookInput::post_tool_use(
            tool_name,
            tool_input,
            tool_output,
            &self.working_dir.to_string_lossy(),
        );

        for hook in matching {
            if let HookResult::Error(err) = self.execute_hook(hook, &input).await {
                tracing::warn!("PostToolUse hook '{}' failed: {}", hook.description, err);
            }
        }
    }

    /// Run all matching session lifecycle hooks (`SessionStart` / `SessionEnd`).
    ///
    /// Functional scope: session events do not have an associated tool and so the
    /// matcher is bypassed; only the event kind is filtered.
    pub async fn run_session_event(&self, event: HookEvent) {
        let matching: Vec<&HookDefinition> = self
            .config
            .hooks
            .iter()
            .filter(|h| h.enabled && h.event == event)
            .collect();

        if matching.is_empty() {
            return;
        }

        let input = HookInput::session_event(event, &self.working_dir.to_string_lossy());

        for hook in matching {
            if let HookResult::Error(err) = self.execute_hook(hook, &input).await {
                tracing::warn!("{} hook '{}' failed: {}", event, hook.description, err);
            }
        }
    }

    /// Spawn `sh -c <hook.command>`, write the input JSON to stdin, and translate
    /// the resulting `Output` into a [`HookResult`].
    ///
    /// Boundary conditions:
    /// - Stdin closure (`BrokenPipe`) is treated as benign: a hook that intentionally
    ///   closes stdin still gets to influence outcome via its exit code.
    /// - Other I/O errors propagate up as [`HookResult::Error`].
    /// - The whole future is wrapped in `tokio::time::timeout(timeout_ms)`; on
    ///   timeout the result is reported as an error rather than a block.
    async fn execute_hook(&self, hook: &HookDefinition, input: &HookInput) -> HookResult {
        let input_json = match serde_json::to_string(input) {
            Ok(json) => json,
            Err(e) => return HookResult::Error(format!("Failed to serialize hook input: {e}")),
        };

        let timeout = Duration::from_millis(hook.timeout_ms);

        let result = tokio::time::timeout(timeout, async {
            let mut child = Command::new("sh")
                .arg("-c")
                .arg(&hook.command)
                .current_dir(&self.working_dir)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()?;

            if let Some(mut stdin) = child.stdin.take() {
                // Hooks receive JSON on stdin, but some hooks intentionally ignore it and
                // close stdin immediately. Treat that as non-fatal and rely on the hook's
                // exit status to decide whether the action should be allowed or blocked.
                match stdin.write_all(input_json.as_bytes()).await {
                    Err(err) if err.kind() != ErrorKind::BrokenPipe => return Err(err),
                    _ => {}
                }
                drop(stdin);
            }

            child.wait_with_output().await
        })
        .await;

        match result {
            Ok(Ok(output)) => {
                let exit_code = output.status.code().unwrap_or(-1);
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                if !stderr.is_empty() {
                    tracing::debug!("Hook stderr: {}", stderr.trim());
                }

                match exit_code {
                    0 => HookResult::Allow,
                    129 => {
                        // Exit code 129 = block (PreToolUse only)
                        let reason = if let Ok(output) = serde_json::from_str::<HookOutput>(&stdout)
                        {
                            output
                                .message
                                .unwrap_or_else(|| "Blocked by hook".to_string())
                        } else if !stdout.trim().is_empty() {
                            stdout.trim().to_string()
                        } else {
                            "Blocked by hook".to_string()
                        };
                        HookResult::Block(reason)
                    }
                    code => HookResult::Error(format!(
                        "Hook exited with code {code}: {}",
                        stderr.trim()
                    )),
                }
            }
            Ok(Err(e)) => HookResult::Error(format!("Failed to run hook: {e}")),
            Err(_) => HookResult::Error(format!("Hook timed out after {}ms", hook.timeout_ms)),
        }
    }
}

/// Internal result of executing a hook.
///
/// Distinct from [`HookAction`] in that it carries a third `Error` variant —
/// callers translate `Error` into "log and continue" so misconfigured hooks
/// cannot wedge the agent.
enum HookResult {
    Allow,
    Block(String),
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::{super::config::HookConfig, *};

    fn make_runner(hooks: Vec<HookDefinition>) -> (HookRunner, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let runner = HookRunner::new(HookConfig { hooks }, tmp.path().to_path_buf());
        (runner, tmp)
    }

    fn make_hook(event: HookEvent, matcher: &str, command: &str) -> HookDefinition {
        HookDefinition {
            event,
            matcher: matcher.to_string(),
            command: command.to_string(),
            description: "test hook".to_string(),
            timeout_ms: 5000,
            enabled: true,
        }
    }

    // Scenario: a benign hook that exits 0 results in `HookAction::Allow`.
    #[tokio::test]
    async fn test_pre_tool_use_allow() {
        let (runner, _tmp) = make_runner(vec![make_hook(
            HookEvent::PreToolUse,
            "read_file",
            "echo '{}'",
        )]);

        let action = runner
            .run_pre_tool_use("read_file", serde_json::json!({}))
            .await;
        assert_eq!(action, HookAction::Allow);
    }

    // Scenario: a hook exiting 129 with a JSON message blocks with that reason.
    #[tokio::test]
    async fn test_pre_tool_use_block() {
        let (runner, _tmp) = make_runner(vec![make_hook(
            HookEvent::PreToolUse,
            "shell",
            r#"exec 0<&-; sleep 0.05; echo "{\"message\":\"dangerous command blocked\"}"; exit 129"#,
        )]);

        let action = runner
            .run_pre_tool_use("shell", serde_json::json!({"command": "rm -rf /"}))
            .await;
        assert!(action.is_blocked());
        if let HookAction::Block(reason) = action {
            assert!(reason.contains("dangerous command blocked"));
        }
    }

    // Scenario: when no hook matches the tool name, the runner short-circuits.
    #[tokio::test]
    async fn test_pre_tool_use_no_matching_hooks() {
        let (runner, _tmp) =
            make_runner(vec![make_hook(HookEvent::PreToolUse, "shell", "exit 129")]);

        let action = runner
            .run_pre_tool_use("read_file", serde_json::json!({}))
            .await;
        assert_eq!(action, HookAction::Allow);
    }

    // Scenario: a hook that exceeds its timeout is killed and treated as Allow,
    // matching the "errors don't block" rule.
    #[tokio::test]
    async fn test_pre_tool_use_timeout() {
        let (runner, _tmp) = make_runner(vec![HookDefinition {
            event: HookEvent::PreToolUse,
            matcher: "*".to_string(),
            command: "sleep 10".to_string(),
            description: "slow hook".to_string(),
            timeout_ms: 100, // 100ms timeout
            enabled: true,
        }]);

        let action = runner
            .run_pre_tool_use("read_file", serde_json::json!({}))
            .await;
        // Timeout results in error, which doesn't block
        assert_eq!(action, HookAction::Allow);
    }

    // Scenario: hooks whose `enabled` flag is false are skipped without spawning.
    #[tokio::test]
    async fn test_disabled_hooks_skipped() {
        let (runner, _tmp) = make_runner(vec![HookDefinition {
            event: HookEvent::PreToolUse,
            matcher: "*".to_string(),
            command: "exit 2".to_string(),
            description: "disabled hook".to_string(),
            timeout_ms: 5000,
            enabled: false,
        }]);

        let action = runner
            .run_pre_tool_use("read_file", serde_json::json!({}))
            .await;
        assert_eq!(action, HookAction::Allow);
    }

    // Scenario: empty config has no hooks; a single-hook config does.
    #[tokio::test]
    async fn test_has_hooks() {
        let (runner, _tmp) = make_runner(vec![]);
        assert!(!runner.has_hooks());

        let (runner, _tmp) = make_runner(vec![make_hook(HookEvent::PreToolUse, "*", "echo ok")]);
        assert!(runner.has_hooks());
    }
}
