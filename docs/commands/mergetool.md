# `libra mergetool`

Run a configured merge-resolution tool for each ordinary content conflict in the current worktree.

## Synopsis

```text
libra mergetool [--tool <tool>]
libra mergetool --tool-help
```

## Behavior

`libra mergetool` reads unresolved index stages and, for an ordinary file conflict, creates an owner-only temporary directory containing:

- `BASE` — stage 1, or an empty file for an add/add conflict;
- `LOCAL` — stage 2 (the current branch's version);
- `REMOTE` — stage 3 (the merged branch's version); and
- `MERGED` — an editable copy of the conflicted worktree file.

When the tool resolves a path, Libra writes `MERGED` back to the worktree and replaces that path's stage 1/2/3 entries with one stage-0 entry. It does not create the final merge commit; use `libra merge --continue` after every conflict is staged.

By default, Libra follows Git's merge-tool success rule: a modification time change to `MERGED` after the tool starts means resolved. If it is unchanged, an interactive terminal is asked for confirmation; stdin EOF or a non-interactive invocation is treated as unresolved and does not stage the path. Set `mergetool.<tool>.trustExitCode=true` only for tools whose exit code reliably represents resolution; in that mode the exit code is the sole decision.

Symlink, mode, and modify/delete conflicts are not sent to a tool. Libra names the path and asks you to resolve that shape manually, rather than silently skipping it. Libra also refuses a conflicted worktree path when it, or any of its parent components, is a symbolic link; it never follows such a link while reading the initial `MERGED` copy, creating a backup, or writing the resolution.

## Tool selection and configuration

`--tool <tool>` wins over `merge.tool`; without either, Libra tries `vimdiff`.

Built-in descriptions are available for `vimdiff`, `nvimdiff`, `meld`, `vscode`, and `opendiff`. Their default executables are looked up on `PATH`; `mergetool.<tool>.path` has priority over that lookup.

For another tool, configure `mergetool.<tool>.cmd`. It is intentionally evaluated by `sh -c`, matching Git, with `BASE`, `LOCAL`, `REMOTE`, and `MERGED` exported as environment variables. The command is trusted local configuration: do not copy it from an untrusted repository or issue tracker. Libra never substitutes a conflict-derived pathname into that command string.

```bash
libra config set merge.tool review
libra config set mergetool.review.cmd 'review-tool "$LOCAL" "$REMOTE" "$MERGED"'
libra config set mergetool.review.trustExitCode true
```

`mergetool.keepBackup` defaults to `true`. A successfully resolved path then preserves the old conflict-marker file as `<path>.orig`; set it to `false` to stop creating new backups.

Only `mergetool.keepBackup` and the per-tool `.cmd`, `.path`, and `.trustExitCode` settings are currently supported. Any other `mergetool.*` key fails closed with an actionable error (DEFER-11), rather than being ignored. GUI discovery details and the full upstream catalog of tool descriptions remain deferred; use a custom `.cmd` when a supported built-in does not fit your tool.

## Exit status

- `0` — all selected conflict paths were resolved and staged, or `--tool-help` was displayed.
- non-zero — there is no unresolved path, the selected tool is unavailable or unknown, a path remains unresolved, or a manual-only conflict shape was found. No unresolved path is staged.

## Examples

```bash
# Inspect the built-in tools without starting a resolver.
libra mergetool --tool-help

# Resolve all ordinary conflicts using the configured merge.tool.
libra mergetool

# Override the configured default once.
libra mergetool --tool vimdiff

# Finish the merge once every conflict is resolved and staged.
libra merge --continue
```
