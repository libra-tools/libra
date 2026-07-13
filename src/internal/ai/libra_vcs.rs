//! Allowlist for Libra VCS commands that external AI tool bridges may execute.
//!
//! Boundary: the allowlist intentionally exposes repository-inspection and safe VCS
//! commands only; raw shell/git execution stays outside this contract. MCP tests cover
//! accepted commands, rejected commands, and argument normalization.

use serde_json::json;

use crate::internal::ai::runtime::hardening::{BlastRadius, SafetyDecision};

pub const ALLOWED_COMMANDS: &[&str] = &[
    "status", "diff", "branch", "log", "show", "show-ref", "ls-files", "add", "commit", "switch",
];

pub const ALLOWED_COMMANDS_DISPLAY: &str =
    "status, diff, branch, log, show, show-ref, ls-files, add, commit, switch";

pub fn run_libra_vcs_tool_guidance() -> String {
    format!(
        "Allowed run_libra_vcs commands: {ALLOWED_COMMANDS_DISPLAY}. Pass flags and paths in \
         args. For working tree state prefer `status --json` or `status --porcelain v2 \
         --untracked-files=all`. Use `ls-files` for index and untracked-path inspection inside \
         the repo, including `ls-files --others --exclude-standard` for ignore-aware untracked \
         files. Do not use Git-only status shorthands like `status -uall` or `status -a`."
    )
}

pub fn unsupported_command_message(prefix: &str, command: &str) -> String {
    format!(
        "unsupported {prefix} command '{command}'; allowed commands: {ALLOWED_COMMANDS_DISPLAY}. \
         For working tree state use `status --json` or `status --porcelain v2 \
         --untracked-files=all`. Use `ls-files` when you need tracked, modified, deleted, or \
         untracked repository paths."
    )
}

pub fn classify_run_libra_vcs_safety(command: &str, args: &[String]) -> SafetyDecision {
    let command = command.trim();
    if command.is_empty() {
        return SafetyDecision::deny(
            "libra_vcs.empty",
            "empty Libra VCS command",
            BlastRadius::Repository,
        );
    }

    if command.chars().any(char::is_whitespace) {
        return SafetyDecision::deny(
            "libra_vcs.invalid_args",
            "run_libra_vcs command must be a single Libra subcommand; pass flags and paths in args",
            BlastRadius::Repository,
        );
    }

    if command_has_control_characters(command)
        || args.iter().any(|arg| command_has_control_characters(arg))
    {
        return SafetyDecision::deny(
            "libra_vcs.invalid_args",
            "Libra VCS command and args must not contain control characters",
            BlastRadius::Repository,
        );
    }

    if !command.is_ascii() || args.iter().any(|arg| !arg.is_ascii()) {
        return SafetyDecision::needs_human(
            "libra_vcs.non_ascii_args",
            "Libra VCS command contains non-ASCII input and needs review",
            BlastRadius::Repository,
        );
    }

    if command != command.to_ascii_lowercase() {
        return SafetyDecision::needs_human(
            "libra_vcs.unknown_command",
            "Libra VCS command is not in the safety corpus",
            BlastRadius::Repository,
        );
    }

    match command {
        "status" => classify_status_safety(args),
        // `libra diff` runs BOTH textconv filters and the external diff driver
        // (`diff.external`) BY DEFAULT, and each is an arbitrary configured shell
        // command. Classify the args first (so a writing/executing arg like
        // `--output`/`--ext-diff` still Denies, and an unknown arg still needs
        // review); then, even when the args are individually read-only, require
        // BOTH `--no-textconv` AND `--no-ext-diff` — without them the diff could
        // run a configured shell command, so it needs human review.
        "diff" => {
            let decision = classify_read_command_safety(args, diff_arg_safety);
            // Only a flag BEFORE the `--` separator counts; after `--` it is a
            // pathspec and does not disable anything.
            let disabled_before_sep = |flag: &str| {
                args.iter()
                    .take_while(|arg| arg.as_str() != "--")
                    .any(|arg| arg == flag)
            };
            let filters_disabled =
                disabled_before_sep("--no-textconv") && disabled_before_sep("--no-ext-diff");
            if decision.rule_name == "libra_vcs.read_only_allowlist" && !filters_disabled {
                SafetyDecision::needs_human(
                    "libra_vcs.diff_default_filters",
                    "Libra VCS diff runs textconv and external diff drivers by default, which can execute configured shell commands; pass --no-textconv --no-ext-diff for a read-only diff",
                    BlastRadius::Repository,
                )
            } else {
                decision
            }
        }
        "log" => classify_read_command_safety(args, log_arg_safety),
        "show" => classify_read_command_safety(args, show_arg_safety),
        "show-ref" => classify_read_command_safety(args, show_ref_arg_safety),
        "ls-files" => classify_read_command_safety(args, ls_files_arg_safety),
        "branch" => classify_branch_safety(args),
        "add" | "commit" | "switch" => SafetyDecision::needs_human(
            "libra_vcs.recoverable_mutation",
            "Libra VCS command mutates repository state and needs approval",
            BlastRadius::Repository,
        ),
        "stash"
            if args
                .iter()
                .map(String::as_str)
                .any(|arg| matches!(arg, "clear" | "drop")) =>
        {
            SafetyDecision::deny(
                "libra_vcs.irreversible_mutation",
                "destructive Libra VCS command is not allowed through run_libra_vcs",
                BlastRadius::Repository,
            )
        }
        "reset" | "rm" | "clean" | "reflog" | "gc" | "tag" | "remote" => SafetyDecision::deny(
            "libra_vcs.irreversible_mutation",
            "destructive Libra VCS command is not allowed through run_libra_vcs",
            BlastRadius::Repository,
        ),
        "push" => SafetyDecision::deny(
            "libra_vcs.irreversible_mutation",
            "networked destructive Libra VCS command is not allowed through run_libra_vcs",
            BlastRadius::Network,
        ),
        _ => SafetyDecision::needs_human(
            "libra_vcs.unknown_command",
            "Libra VCS command is not in the safety corpus",
            BlastRadius::Repository,
        ),
    }
}

pub fn format_run_libra_vcs_safety_message(
    command: &str,
    args: &[String],
    decision: &SafetyDecision,
) -> String {
    let status = if decision.is_deny() {
        "denied"
    } else {
        "approval_required"
    };
    let body = json!({
        "status": status,
        "command": command,
        "args": args,
        "approval_required": decision.is_needs_human(),
        "rule_name": decision.rule_name,
        "reason": decision.reason,
        "blast_radius": decision.blast_radius.to_string(),
        "guidance": run_libra_vcs_tool_guidance(),
    });

    match serde_json::to_string(&body) {
        Ok(body) => body,
        Err(error) => format!(
            "{{\"status\":\"{status}\",\"reason\":\"failed to serialize Libra VCS safety decision: {error}\"}}"
        ),
    }
}

pub fn normalize_tool_args(command: &str, args: &[String]) -> Result<Vec<String>, String> {
    if command != "status" {
        return Ok(args.to_vec());
    }

    let mut normalized = Vec::with_capacity(args.len());
    for arg in args {
        match arg.as_str() {
            "-uall" => normalized.push("--untracked-files=all".to_string()),
            "-unormal" => normalized.push("--untracked-files=normal".to_string()),
            "-uno" => normalized.push("--untracked-files=no".to_string()),
            "-a" => {
                return Err(
                    "run_libra_vcs status does not support '-a'; use '--untracked-files=all' \
                     when you need every untracked file listed"
                        .to_string(),
                );
            }
            _ => normalized.push(arg.clone()),
        }
    }

    Ok(normalized)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArgSafety {
    Allow,
    Deny,
    Unknown,
}

fn classify_status_safety(args: &[String]) -> SafetyDecision {
    match normalize_tool_args("status", args) {
        Ok(normalized_args) if status_args_are_read_only(&normalized_args) => {
            allow_read_only_libra_vcs()
        }
        Ok(_) => SafetyDecision::needs_human(
            "libra_vcs.unknown_args",
            "status arguments are not in the read-only safety corpus",
            BlastRadius::Repository,
        ),
        Err(error) => {
            SafetyDecision::deny("libra_vcs.invalid_args", error, BlastRadius::Repository)
        }
    }
}

fn classify_read_command_safety(
    args: &[String],
    classify_arg: fn(&[String], usize) -> (ArgSafety, usize),
) -> SafetyDecision {
    let mut idx = 0;
    while idx < args.len() {
        let (safety, next_idx) = classify_arg(args, idx);
        match safety {
            ArgSafety::Allow => idx = next_idx,
            ArgSafety::Deny => {
                return SafetyDecision::deny(
                    "libra_vcs.irreversible_mutation",
                    "Libra VCS read command includes an argument that writes, executes external helpers, or deletes state",
                    BlastRadius::Repository,
                );
            }
            ArgSafety::Unknown => {
                return SafetyDecision::needs_human(
                    "libra_vcs.unknown_args",
                    "Libra VCS read command uses arguments that need review",
                    BlastRadius::Repository,
                );
            }
        }
    }

    allow_read_only_libra_vcs()
}

fn classify_branch_safety(args: &[String]) -> SafetyDecision {
    if args.iter().map(String::as_str).any(branch_delete_flag) {
        if branch_delete_targets_protected_branch(args) {
            return SafetyDecision::deny(
                "libra_vcs.irreversible_mutation",
                "protected branch deletion is not allowed through run_libra_vcs",
                BlastRadius::Repository,
            );
        }
        return SafetyDecision::needs_human(
            "libra_vcs.recoverable_mutation",
            "branch deletion mutates repository state and needs approval",
            BlastRadius::Repository,
        );
    }

    if args.iter().map(String::as_str).any(branch_force_flag) {
        return SafetyDecision::needs_human(
            "libra_vcs.recoverable_mutation",
            "branch force update mutates repository state and needs approval",
            BlastRadius::Repository,
        );
    }

    if branch_args_are_read_only(args) {
        return allow_read_only_libra_vcs();
    }

    SafetyDecision::needs_human(
        "libra_vcs.recoverable_mutation",
        "branch command may create or update repository state and needs approval",
        BlastRadius::Repository,
    )
}

fn allow_read_only_libra_vcs() -> SafetyDecision {
    SafetyDecision::allow(
        "libra_vcs.read_only_allowlist",
        "read-only Libra VCS command is allowlisted",
        BlastRadius::Repository,
    )
}

fn status_args_are_read_only(args: &[String]) -> bool {
    let mut idx = 0;
    while idx < args.len() {
        let arg = args[idx].as_str();
        match arg {
            "--" => return true,
            "--json" | "-J" | "--machine" | "--short" | "-s" | "--branch" | "-b"
            | "--ahead-behind" | "--no-ahead-behind" | "--renames" | "--no-renames"
            | "--show-stash" | "--ignored" => idx += 1,
            "--porcelain" => {
                if args
                    .get(idx + 1)
                    .is_some_and(|value| porcelain_version(value))
                {
                    idx += 2;
                } else {
                    idx += 1;
                }
            }
            "--untracked-files"
                if args
                    .get(idx + 1)
                    .is_some_and(|value| untracked_files_mode(value)) =>
            {
                idx += 2;
            }
            "--untracked-files" => return false,
            "--ignored-mode" if args.get(idx + 1).is_some_and(|value| ignored_mode(value)) => {
                idx += 2;
            }
            "--ignored-mode" => return false,
            _ if arg.starts_with("--porcelain=")
                && porcelain_version(arg.trim_start_matches("--porcelain=")) =>
            {
                idx += 1;
            }
            _ if arg.starts_with("--porcelain=") => return false,
            _ if arg.starts_with("--untracked-files=")
                && untracked_files_mode(arg.trim_start_matches("--untracked-files=")) =>
            {
                idx += 1;
            }
            _ if arg.starts_with("--untracked-files=") => return false,
            _ if arg.starts_with("--ignored=")
                && ignored_mode(arg.trim_start_matches("--ignored=")) =>
            {
                idx += 1;
            }
            _ if arg.starts_with("--ignored=") => return false,
            _ if !arg.starts_with('-') => idx += 1,
            _ => return false,
        }
    }

    true
}

fn diff_arg_safety(args: &[String], idx: usize) -> (ArgSafety, usize) {
    let arg = args[idx].as_str();
    if diff_arg_denies(arg) {
        return (ArgSafety::Deny, idx + 1);
    }
    if arg == "--" {
        return (ArgSafety::Allow, args.len());
    }
    if matches!(
        arg,
        "--stat"
            | "--shortstat"
            | "--numstat"
            | "--summary"
            | "--compact-summary"
            | "--name-only"
            | "--name-status"
            | "--cached"
            | "--staged"
            | "--check"
            | "--color"
            | "--no-color"
            | "--patch"
            | "-p"
            | "--word-diff"
            | "--color-words"
            | "--no-ext-diff"
            | "--no-textconv"
            | "--histogram"
            | "--patience"
            | "--minimal"
    ) || arg.starts_with("--stat=")
        || arg.starts_with("--color=")
        || arg.starts_with("--word-diff=")
        || arg.starts_with("--diff-filter=")
        || arg.starts_with("--submodule=")
        || arg.starts_with("--relative=")
        || (arg.len() > 2 && (arg.starts_with("-S") || arg.starts_with("-G")))
        || !arg.starts_with('-')
    {
        return (ArgSafety::Allow, idx + 1);
    }
    if matches!(
        arg,
        "--diff-filter" | "--submodule" | "--relative" | "-S" | "-G"
    ) && args.get(idx + 1).is_some()
    {
        return (ArgSafety::Allow, idx + 2);
    }
    (ArgSafety::Unknown, idx + 1)
}

fn log_arg_safety(args: &[String], idx: usize) -> (ArgSafety, usize) {
    let arg = args[idx].as_str();
    if arg == "--" {
        return (ArgSafety::Allow, args.len());
    }
    if matches!(
        arg,
        "--oneline"
            | "--stat"
            | "--shortstat"
            | "--patch-with-stat"
            | "--numstat"
            | "--summary"
            | "--patch"
            | "-p"
            | "--graph"
            | "--decorate"
            | "--no-decorate"
            | "--all"
            | "--branches"
            | "--remotes"
            | "--tags"
            | "--date-order"
            | "--topo-order"
            | "--reverse"
            | "--no-merges"
            | "--merges"
    ) || arg.starts_with("--max-count=")
        || arg.starts_with("--since=")
        || arg.starts_with("--until=")
        || arg.starts_with("--author=")
        || arg.starts_with("--grep=")
        || arg.starts_with("--format=")
        || arg.starts_with("--pretty=")
        || arg.starts_with("--decorate=")
        || numeric_short_limit(arg)
        || !arg.starts_with('-')
    {
        return (ArgSafety::Allow, idx + 1);
    }
    if matches!(
        arg,
        "--max-count"
            | "-n"
            | "--since"
            | "--until"
            | "--author"
            | "--grep"
            | "--format"
            | "--pretty"
    ) && args.get(idx + 1).is_some()
    {
        return (ArgSafety::Allow, idx + 2);
    }
    (ArgSafety::Unknown, idx + 1)
}

fn show_arg_safety(args: &[String], idx: usize) -> (ArgSafety, usize) {
    let arg = args[idx].as_str();
    if show_arg_denies(arg) {
        return (ArgSafety::Deny, idx + 1);
    }
    if arg == "--" {
        return (ArgSafety::Allow, args.len());
    }
    if matches!(
        arg,
        "--stat"
            | "--shortstat"
            | "--patch-with-stat"
            | "--numstat"
            | "--summary"
            | "--name-only"
            | "--name-status"
            | "--no-patch"
            | "--patch"
            | "-p"
            | "--color"
            | "--no-color"
    ) || arg.starts_with("--format=")
        || arg.starts_with("--pretty=")
        || arg.starts_with("--color=")
        || !arg.starts_with('-')
    {
        return (ArgSafety::Allow, idx + 1);
    }
    if matches!(arg, "--format" | "--pretty") && args.get(idx + 1).is_some() {
        return (ArgSafety::Allow, idx + 2);
    }
    (ArgSafety::Unknown, idx + 1)
}

fn show_ref_arg_safety(args: &[String], idx: usize) -> (ArgSafety, usize) {
    let arg = args[idx].as_str();
    if matches!(
        arg,
        "--heads"
            | "--tags"
            | "--verify"
            | "--head"
            | "--dereference"
            | "-d"
            | "--exists"
            | "--exclude-existing"
            | "--quiet"
            | "-q"
            | "--hash"
            | "--abbrev"
    ) || arg.starts_with("--hash=")
        || arg.starts_with("--abbrev=")
        || !arg.starts_with('-')
    {
        return (ArgSafety::Allow, idx + 1);
    }
    (ArgSafety::Unknown, idx + 1)
}

fn ls_files_arg_safety(args: &[String], idx: usize) -> (ArgSafety, usize) {
    let arg = args[idx].as_str();
    if matches!(
        arg,
        "--cached"
            | "-c"
            | "--deleted"
            | "-d"
            | "--modified"
            | "-m"
            | "--stage"
            | "-s"
            | "--others"
            | "-o"
            | "--exclude-standard"
            | "--error-unmatch"
            | "--json"
            | "-J"
            | "--machine"
    ) || arg.starts_with("--json=")
        || arg.starts_with("-J=")
        || !arg.starts_with('-')
    {
        return (ArgSafety::Allow, idx + 1);
    }

    // Clap expands grouped boolean shorts (e.g. `-dm` == `-d -m`), so a single
    // `-…` token is read-only iff every letter is an allowlisted read-only short
    // (`-z` is intentionally excluded, keeping `-z`/`-dz` unknown).
    if !arg.starts_with("--")
        && let Some(group) = arg.strip_prefix('-')
        && !group.is_empty()
        && group
            .chars()
            .all(|c| matches!(c, 'c' | 'd' | 'm' | 'o' | 's' | 'J'))
    {
        return (ArgSafety::Allow, idx + 1);
    }

    (ArgSafety::Unknown, idx + 1)
}

fn branch_args_are_read_only(args: &[String]) -> bool {
    if args.is_empty() {
        return true;
    }

    let list_mode = args
        .iter()
        .map(String::as_str)
        .any(|arg| matches!(arg, "--list" | "-l" | "--all" | "-a" | "--remotes" | "-r"));

    let mut idx = 0;
    while idx < args.len() {
        let arg = args[idx].as_str();
        match arg {
            "--list" | "-l" | "--show-current" | "-a" | "--all" | "-r" | "--remotes" | "-v"
            | "-vv" | "--merged" | "--no-merged" => idx += 1,
            "--format" | "--sort" | "--contains" | "--points-at" if args.get(idx + 1).is_some() => {
                idx += 2;
            }
            "--format" | "--sort" | "--contains" | "--points-at" => return false,
            _ if arg.starts_with("--format=")
                || arg.starts_with("--sort=")
                || arg.starts_with("--contains=")
                || arg.starts_with("--points-at=") =>
            {
                idx += 1;
            }
            _ if list_mode && !arg.starts_with('-') => idx += 1,
            _ => return false,
        }
    }

    true
}

fn diff_arg_denies(arg: &str) -> bool {
    matches!(arg, "--output" | "--ext-diff" | "--textconv") || arg.starts_with("--output=")
}

fn show_arg_denies(arg: &str) -> bool {
    matches!(arg, "--output") || arg.starts_with("--output=")
}

fn branch_delete_targets_protected_branch(args: &[String]) -> bool {
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        let raw = arg.as_str();
        if matches!(
            raw,
            "--format" | "--sort" | "--contains" | "--points-at" | "-m" | "--move"
        ) {
            skip_next = true;
            continue;
        }
        // Inline forms like `--delete=main` / `-d=main` / `-D=main` carry the
        // branch name as the value half — extract it instead of skipping the
        // whole arg, otherwise a protected-branch deletion routes through the
        // `needs_human` path instead of `deny`.
        if let Some(value) = inline_delete_value(raw) {
            if is_protected_branch(value) {
                return true;
            }
            continue;
        }
        if raw.starts_with('-') {
            continue;
        }
        if is_protected_branch(raw) {
            return true;
        }
    }
    false
}

fn inline_delete_value(arg: &str) -> Option<&str> {
    // Long-flag inline forms: `--delete=name` (safe delete) and
    // `--delete-force=name` (force delete, the long form of `-D`).
    if let Some(rest) = arg.strip_prefix("--delete-force=") {
        return Some(rest);
    }
    if let Some(rest) = arg.strip_prefix("--delete=") {
        return Some(rest);
    }
    // Short-flag inline forms (`-d=name`, `-D=name`) — clap normally splits
    // these but a hand-written argv may pass them whole.
    if let Some(rest) = arg.strip_prefix("-d=") {
        return Some(rest);
    }
    if let Some(rest) = arg.strip_prefix("-D=") {
        return Some(rest);
    }
    None
}

fn is_protected_branch(branch: &str) -> bool {
    matches!(branch, "main" | "master" | "trunk" | "develop") || branch.starts_with("release/")
}

fn branch_delete_flag(arg: &str) -> bool {
    matches!(arg, "-d" | "-D" | "--delete" | "--delete-force")
        || arg.starts_with("--delete=")
        || arg.starts_with("--delete-force=")
        || short_flag_group_contains(arg, 'd')
        || short_flag_group_contains(arg, 'D')
}

fn branch_force_flag(arg: &str) -> bool {
    matches!(arg, "-f" | "--force") || short_flag_group_contains(arg, 'f')
}

fn porcelain_version(value: &str) -> bool {
    matches!(value, "v1" | "v2" | "1" | "2")
}

fn untracked_files_mode(value: &str) -> bool {
    matches!(value, "all" | "normal" | "no")
}

fn ignored_mode(value: &str) -> bool {
    matches!(value, "traditional" | "matching" | "no")
}

fn numeric_short_limit(arg: &str) -> bool {
    arg.len() > 1
        && arg.starts_with('-')
        && !arg.starts_with("--")
        && arg[1..].chars().all(|ch| ch.is_ascii_digit())
}

fn short_flag_group_contains(arg: &str, target: char) -> bool {
    arg.starts_with('-') && !arg.starts_with("--") && arg.chars().skip(1).any(|c| c == target)
}

fn command_has_control_characters(value: &str) -> bool {
    value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_guidance_mentions_allowed_commands_and_read_only_hints() {
        let guidance = run_libra_vcs_tool_guidance();

        assert!(guidance.contains(ALLOWED_COMMANDS_DISPLAY));
        assert!(guidance.contains("status --json"));
        assert!(guidance.contains("ls-files"));
        assert!(guidance.contains("--exclude-standard"));
        assert!(guidance.contains("status -uall"));
    }

    #[test]
    fn unsupported_command_message_is_actionable() {
        let message = unsupported_command_message("run_libra_vcs", "remote");

        assert!(message.contains("allowed commands"));
        assert!(message.contains("status --json"));
        assert!(message.contains("ls-files"));
    }

    #[test]
    fn normalize_status_args_rewrites_git_untracked_shorthand() {
        let args = normalize_tool_args("status", &["-uall".to_string()]).unwrap();

        assert_eq!(args, vec!["--untracked-files=all"]);
    }

    #[test]
    fn normalize_status_args_rejects_invalid_status_a_with_hint() {
        let error = normalize_tool_args("status", &["-a".to_string()]).unwrap_err();

        assert!(error.contains("--untracked-files=all"));
    }
}
