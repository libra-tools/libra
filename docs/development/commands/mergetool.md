# `libra mergetool` implementation notes

## Surface

`Commands::Mergetool` dispatches to `command::mergetool::execute_safe`. It is a worktree mutation: successful paths change the worktree and the current worktree's index, but never refs or merge state. `--tool-help` renders the five built-in descriptions (`vimdiff`, `nvimdiff`, `meld`, `vscode`, `opendiff`) and their default PATH availability.

## Resolution protocol

The command enumerates unresolved paths using `merge::unresolved_conflicted_paths`, loads stage 1/2/3 blobs, and creates a mode-0700 RAII temporary directory. Each path gets `BASE`, `LOCAL`, `REMOTE`, and `MERGED`; a missing stage 1 is represented by an empty `BASE` file for add/add conflicts.

Only ordinary regular-file conflicts with stages 2 and 3 enter the tool protocol. A missing side is a modify/delete conflict; a symbolic-link stage or a different file mode is a manual-only conflict. Both cases fail clearly before a tool runs and leave index/worktree state intact. The worktree input is read through `beneath::read_file_beneath` from a pinned root, so a symbolic-link leaf or ancestor is refused instead of followed.

`mergetool.<tool>.cmd` is invoked as `sh -c <configured-command>`, with the four paths set only as environment variables. This is an intentional trusted-configuration boundary matching git@3cb9185f6 `Documentation/config/mergetool.adoc:5` and `git-mergetool--lib.sh`: conflict paths must never be interpolated into the shell command. Built-in tools use `mergetool.<tool>.path` before their PATH executable.

Absent `mergetool.<tool>.trustExitCode`, Git's `check_unchanged` behavior applies: only a `MERGED` mtime advance proves resolution. An unchanged file asks on an interactive terminal; EOF/non-interactive input is unresolved. When `trustExitCode=true`, the exit status is the only resolution signal.

On a resolved path, `mergetool.keepBackup` defaults to true and writes `<path>.orig`, then the command writes the temporary merged bytes to the worktree, stores the blob, removes stages 1/2/3, and writes a fresh stage-0 entry. Backup and resolution writes use `beneath::write_regular_file_beneath`: the pinned parent descriptor and no-follow leaf open prevent a concurrent path swap from escaping the worktree, and the descriptor is checked as a regular file before truncation. The stage-0 entry is built from the already-verified temporary `MERGED` blob, retains the conflict mode, and has zero stat fields so later status checks compare its content rather than reopening the just-written path. Removing obsolete conflict stages explicitly matters because the low-level index map keys entries by `(name, stage)`.

## Deferred configuration

MG-14 intentionally supports only `merge.tool`, `mergetool.<tool>.cmd`, `.path`, `.trustExitCode`, and `mergetool.keepBackup`. The command enumerates every configured `mergetool.*` key through local/global/system and legacy config stores; any other key (for example `hideResolved`, prompt/GUI choices, temporary-file preservation, and tool-specific GUI switches) is DEFER-11 and fails closed rather than becoming a silent no-op. The complete upstream tool catalog is DEFER-03.

## Tests

- `command::mergetool::tests` pins temporary input extraction and 0700 directory permissions.
- `command::mergetool_test` covers custom shell variables, mtime versus trusted-exit decisions, stdin EOF, stage cleanup, path priority, default/explicit tool selection, backups, no-work/unknown-tool diagnostics, tool help, manual-only conflict classes, and both pre-existing and tool-time symbolic-link ancestor swaps.
