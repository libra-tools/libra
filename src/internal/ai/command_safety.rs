//! Shell command safety classification — plan-20260920 RC-09.
//!
//! KEEP `sandbox` / `automation` call this module instead of the deleted
//! Code-era `tools::utils::classify_ai_command_safety`. Both Shell and
//! LibraVcs arms live here after RC-23.

use std::{ffi::OsStr, path::Path};

use crate::internal::ai::hardening::{
    BlastRadius, CommandSafetySurface, SafetyDecision, SafetyDisposition,
};

/// Provider-neutral classifier used by KEEP sandbox / automation and tests.
pub fn classify_ai_command_safety(
    surface: CommandSafetySurface,
    command: &str,
    args: &[String],
) -> SafetyDecision {
    match surface {
        CommandSafetySurface::Shell => classify_shell_command_safety(command),
        CommandSafetySurface::LibraVcs => classify_run_libra_vcs_safety(command, args),
    }
}

/// Returns true when a shell command appears to invoke Git as a version-control
/// executable. This is deliberately conservative: Libra-managed agents must use
/// Libra VCS tools instead of shelling out to `git`.
pub fn command_invokes_git_version_control(command: &str) -> bool {
    shlex::split(command).is_some_and(|words| shell_words_invoke_git(&words))
}

fn shell_words_invoke_git(words: &[String]) -> bool {
    let mut start = 0;
    for (idx, word) in words.iter().enumerate() {
        if matches!(word.as_str(), "&&" | "||" | ";" | "|") {
            if shell_segment_invokes_git(&words[start..idx]) {
                return true;
            }
            start = idx + 1;
        }
    }

    shell_segment_invokes_git(&words[start..])
}

fn shell_segment_invokes_git(segment: &[String]) -> bool {
    let mut idx = 0;
    while segment
        .get(idx)
        .is_some_and(|word| word.contains('=') && !word.starts_with('-'))
    {
        idx += 1;
    }

    while matches!(
        segment.get(idx).map(String::as_str),
        Some("command" | "sudo" | "env")
    ) {
        idx += 1;
        while segment
            .get(idx)
            .is_some_and(|word| word.contains('=') && !word.starts_with('-'))
        {
            idx += 1;
        }
    }

    segment
        .get(idx)
        .and_then(|word| executable_name(word))
        .is_some_and(|name| name == "git")
}

pub fn classify_shell_command_safety(command: &str) -> SafetyDecision {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return SafetyDecision::deny("shell.empty", "empty shell command", BlastRadius::Unknown);
    }

    if !trimmed.is_ascii() {
        return SafetyDecision::needs_human(
            "shell.non_ascii_command",
            "shell command contains non-ASCII characters and needs review",
            BlastRadius::Unknown,
        );
    }

    if command_invokes_git_version_control(trimmed) {
        return SafetyDecision::deny(
            "shell.direct_git_forbidden",
            "AI shell tools must use Libra VCS tools instead of invoking git directly",
            BlastRadius::Repository,
        );
    }

    let lower = trimmed.to_ascii_lowercase();
    if network_command_piped_to_shell(&lower) {
        return SafetyDecision::deny(
            "shell.network_code_execution",
            "network download piped into a shell is not allowed",
            BlastRadius::Network,
        );
    }

    if lower.contains("$(") || lower.contains('`') {
        return SafetyDecision::needs_human(
            "shell.dynamic_evaluation",
            "shell command uses dynamic evaluation or command substitution",
            BlastRadius::System,
        );
    }

    if contains_redirection_or_pipeline(&lower) {
        return SafetyDecision::needs_human(
            "shell.redirection_or_pipeline",
            "shell command combines commands or redirects data and needs review",
            shell_meta_blast_radius(&lower),
        );
    }

    let Some(parts) = shlex::split(trimmed).filter(|parts| !parts.is_empty()) else {
        return SafetyDecision::needs_human(
            "shell.unparseable",
            "shell command could not be parsed into plain words",
            BlastRadius::Unknown,
        );
    };

    classify_shell_words(&parts)
}

fn classify_shell_words(parts: &[String]) -> SafetyDecision {
    let Some(cmd) = executable_name(parts.first().map(String::as_str).unwrap_or_default()) else {
        return SafetyDecision::needs_human(
            "shell.unparseable",
            "shell command has no executable name",
            BlastRadius::Unknown,
        );
    };

    if matches!(cmd.as_str(), "command" | "env")
        && let Some(decision) = classify_shell_wrapper_words(&cmd, &parts[1..])
    {
        return decision;
    }

    if is_shell_interpreter(&cmd)
        && let Some(script) = shell_c_script(&parts[1..])
    {
        let decision = classify_shell_command_safety(script);
        return match decision.disposition {
            SafetyDisposition::Deny => decision,
            _ => SafetyDecision::needs_human(
                "shell.dynamic_evaluation",
                "shell interpreter command uses -c and needs review",
                BlastRadius::System,
            ),
        };
    }

    if cmd == "git" {
        return SafetyDecision::deny(
            "shell.direct_git_forbidden",
            "AI shell tools must use Libra VCS tools instead of invoking git directly",
            BlastRadius::Repository,
        );
    }

    if cmd == "sudo" {
        if let Some(blast_radius) = destructive_shell_blast_radius(&parts[1..]) {
            return SafetyDecision::deny(
                "shell.destructive_command",
                "privileged destructive shell command is not allowed",
                match blast_radius {
                    BlastRadius::Workspace => BlastRadius::System,
                    other => other,
                },
            );
        }
        return SafetyDecision::needs_human(
            "shell.privileged_execution",
            "privileged shell command needs human approval",
            BlastRadius::System,
        );
    }

    if let Some(blast_radius) = destructive_shell_blast_radius(parts) {
        return SafetyDecision::deny(
            "shell.destructive_command",
            "destructive shell command is not allowed",
            blast_radius,
        );
    }

    if cmd == "libra" {
        return classify_shell_libra_command(parts);
    }

    if is_network_executable(&cmd) {
        return SafetyDecision::needs_human(
            "shell.network_access",
            "shell command requires network access",
            BlastRadius::Network,
        );
    }

    if shell_words_are_read_only(&cmd, &parts[1..]) {
        return SafetyDecision::allow(
            "shell.read_only_allowlist",
            "read-only shell command is allowlisted",
            BlastRadius::Workspace,
        );
    }

    SafetyDecision::needs_human(
        "shell.workspace_mutation_or_execution",
        "shell command may execute code or mutate the workspace",
        BlastRadius::Workspace,
    )
}

fn classify_shell_wrapper_words(wrapper: &str, args: &[String]) -> Option<SafetyDecision> {
    let mut idx = 0;
    if wrapper == "env" {
        idx = skip_env_prefix(args);
    }
    if idx >= args.len() {
        return Some(SafetyDecision::needs_human(
            "shell.workspace_mutation_or_execution",
            "shell wrapper command without an executable needs review",
            BlastRadius::Workspace,
        ));
    }
    Some(classify_shell_words(&args[idx..]))
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

fn classify_shell_libra_command(parts: &[String]) -> SafetyDecision {
    let Some(subcommand) = parts.get(1).map(String::as_str) else {
        return SafetyDecision::needs_human(
            "shell.workspace_mutation_or_execution",
            "libra command without a subcommand needs review",
            BlastRadius::Repository,
        );
    };
    let decision = classify_run_libra_vcs_safety(subcommand, &parts[2..]);
    if decision.is_allow() {
        SafetyDecision::allow(
            "shell.libra_read_only",
            "read-only libra command is allowlisted",
            BlastRadius::Repository,
        )
    } else {
        decision
    }
}

fn contains_redirection_or_pipeline(command: &str) -> bool {
    command.contains(['>', '<', '|', ';']) || command.contains("&&") || command.contains("||")
}

fn shell_meta_blast_radius(command: &str) -> BlastRadius {
    if command.contains('>') || command.contains('<') {
        BlastRadius::Workspace
    } else {
        BlastRadius::Unknown
    }
}

fn network_command_piped_to_shell(command: &str) -> bool {
    (command.starts_with("curl ")
        || command.starts_with("wget ")
        || command.contains(" curl ")
        || command.contains(" wget "))
        && command.contains('|')
        && (command.contains(" sh") || command.contains(" bash"))
}

fn shell_words_are_read_only(cmd: &str, args: &[String]) -> bool {
    match cmd {
        "cat" | "cut" | "echo" | "false" | "grep" | "head" | "id" | "ls" | "nl" | "paste"
        | "pwd" | "rev" | "seq" | "stat" | "tail" | "tr" | "true" | "uname" | "uniq" | "wc"
        | "which" | "whoami" => true,
        "rg" => !args.iter().map(String::as_str).any(|arg| {
            matches!(arg, "--pre" | "--hostname-bin" | "--search-zip" | "-z")
                || arg.starts_with("--pre=")
                || arg.starts_with("--hostname-bin=")
        }),
        "find" => !args.iter().map(String::as_str).any(|arg| {
            matches!(
                arg,
                "-exec"
                    | "-execdir"
                    | "-ok"
                    | "-okdir"
                    | "-delete"
                    | "-fls"
                    | "-fprint"
                    | "-fprint0"
                    | "-fprintf"
            )
        }),
        "sed" => args
            .first()
            .is_some_and(|arg| arg == "-n" && args.get(1).is_some_and(|arg| sed_print_arg(arg))),
        _ => false,
    }
}

fn destructive_shell_blast_radius(parts: &[String]) -> Option<BlastRadius> {
    let cmd = executable_name(parts.first().map(String::as_str)?)?;
    let args = &parts[1..];

    match cmd.as_str() {
        "rm" if rm_args_are_recursive_force(args) => {
            if args
                .iter()
                .any(|arg| arg == "/" || arg.starts_with("/dev/"))
            {
                Some(BlastRadius::System)
            } else {
                Some(BlastRadius::Workspace)
            }
        }
        "chmod"
            if args.iter().any(|arg| arg == "-R" || arg.starts_with("-R"))
                && args.iter().any(|arg| arg == "777") =>
        {
            Some(BlastRadius::Workspace)
        }
        "chown" if args.iter().any(|arg| arg == "-R" || arg.starts_with("-R")) => {
            Some(BlastRadius::Workspace)
        }
        "dd" if args.iter().any(|arg| arg.starts_with("of=/dev/")) => Some(BlastRadius::System),
        "mkfs" | "mkfs.ext4" | "shutdown" | "reboot" | "poweroff" => Some(BlastRadius::System),
        _ => None,
    }
}

fn rm_args_are_recursive_force(args: &[String]) -> bool {
    let recursive = args.iter().map(String::as_str).any(|arg| {
        matches!(arg, "-r" | "-R" | "--recursive") || short_flag_group_contains(arg, 'r')
    });
    let force = args
        .iter()
        .map(String::as_str)
        .any(|arg| matches!(arg, "-f" | "--force") || short_flag_group_contains(arg, 'f'));
    recursive && force
}

fn is_network_executable(cmd: &str) -> bool {
    matches!(
        cmd,
        "curl" | "wget" | "ssh" | "scp" | "sftp" | "nc" | "netcat" | "gh"
    )
}

fn executable_name(command: &str) -> Option<String> {
    Path::new(command)
        .file_name()
        .and_then(OsStr::to_str)
        .map(|name| name.trim_end_matches(".exe").to_ascii_lowercase())
}

fn short_flag_group_contains(arg: &str, target: char) -> bool {
    arg.starts_with('-') && !arg.starts_with("--") && arg.chars().skip(1).any(|c| c == target)
}

fn sed_print_arg(arg: &str) -> bool {
    let Some(core) = arg.strip_suffix('p') else {
        return false;
    };
    let parts: Vec<&str> = core.split(',').collect();
    match parts.as_slice() {
        [one] => !one.is_empty() && one.chars().all(|ch| ch.is_ascii_digit()),
        [start, end] => {
            !start.is_empty()
                && !end.is_empty()
                && start.chars().all(|ch| ch.is_ascii_digit())
                && end.chars().all(|ch| ch.is_ascii_digit())
        }
        _ => false,
    }
}

// --- Libra VCS safety corpus (moved from libra_vcs.rs in RC-23) ---

pub const ALLOWED_COMMANDS: &[&str] = &[
    "status", "diff", "branch", "log", "show", "show-ref", "ls-files", "add", "commit", "switch",
];

pub const ALLOWED_COMMANDS_DISPLAY: &str =
    "status, diff, branch, log, show, show-ref, ls-files, add, commit, switch";

/// Classify a `run_libra_vcs` tool invocation after RC-23 deleted `libra_vcs.rs`.
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

fn normalize_tool_args(command: &str, args: &[String]) -> Result<Vec<String>, String> {
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
        || arg.starts_with("--word-diff-regex=")
        || arg.starts_with("--color-words=")
        || arg.starts_with("--algorithm=")
        || arg.starts_with("--anchored=")
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
        "--algorithm"
            | "--anchored"
            | "--diff-filter"
            | "--submodule"
            | "--relative"
            | "--word-diff-regex"
            | "-S"
            | "-G"
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

fn command_has_control_characters(value: &str) -> bool {
    value.chars().any(char::is_control)
}
