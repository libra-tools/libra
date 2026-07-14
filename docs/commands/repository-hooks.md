# Repository hooks

Libra can run repository-local lifecycle hooks from `.libra/hooks`. These are
separate from the hidden `libra hooks` command used by external AI-agent
integrations. Libra deliberately does not read `.git/hooks` or
`core.hooksPath`.

## Discovery and trust boundary

For each lifecycle name, Libra looks for the extensionless filename first,
then `<name>.sh` on Unix or `<name>.ps1` on Windows. Only one file runs. A
present but unsafe higher-priority candidate fails closed instead of falling
through. The hooks directory and hook file must be real paths, not symbolic
links; the file must be regular and, on Unix, executable. The exact inert
`pre-commit` templates installed by `libra init` are treated as examples rather
than executable policy. A hook file larger than 16 MiB is rejected before it is
copied or executed.

Hooks are arbitrary code, so Libra copies the selected file into a private,
read-only execution location and launches it with structured arguments inside
the required repository sandbox. The hook may write ordinary files within the
current worktree, but `.git`, `.libra`, `.codex`, and `.agents` are protected.
Only `prepare-commit-msg` and `commit-msg` receive write access to the current
worktree's `.libra/COMMIT_EDITMSG`. Writes outside the worktree and network
access are denied. Runtime is limited to 15 minutes and captured output to 1
MiB. If the required sandbox backend is unavailable, a customized hook fails
closed rather than running unsandboxed.

Hook processes do not inherit arbitrary caller environment variables. Libra
clears the environment, then passes only present `PATH`, home/profile, locale,
terminal/timezone, and Windows process-location variables, plus the hook
variables below and command-private temp variables. API tokens, cloud
credentials, agent sockets, and other caller-specific variables are not
forwarded to repository-controlled code.

The current Windows restricted-token backend is not implemented. Libra skips
its exact shipped no-op PowerShell template, so ordinary initialized
repositories continue to work; a customized Windows repository hook fails
closed until that backend is available. Use an escape valve below only after
reviewing the policy impact.

Each hook receives `LIBRA_HOOK_NAME`, `LIBRA_DIR` (the worktree-private
metadata directory), `LIBRA_COMMON_DIR` (shared repository metadata),
`LIBRA_HOOK_SOURCE` (the original selected file), and `LIBRA_WORK_TREE`.

## Lifecycle

| Hook | Timing and arguments | Failure behavior |
|------|----------------------|------------------|
| `pre-commit` | Before commit message handling; no arguments. | Blocks the commit. |
| `prepare-commit-msg` | Before the editor for `commit`, or after `pre-merge-commit` for an automatic merge commit; `<message-file> [source [commit]]`. Sources are `message`, `template`, `merge`, or `commit`. It may edit the message file. | Blocks the commit. |
| `commit-msg` | After editing/trailers, or after merge-message preparation; `<message-file>`. It may edit the message file. | Blocks the commit. |
| `post-commit` | After a successful `commit` or automatic merge commit; no arguments. On amend it runs before `post-rewrite`. | Advisory: warns but does not roll back. |
| `post-checkout` | After a state-changing checkout or switch; `<old-oid> <new-oid> <branch-flag>`, where the flag is `1` for branch/detached checkout and `0` for path restoration. Already-on/show-current operations do not run it. | Advisory. |
| `pre-rebase` | Before a new rebase, including `pull --rebase`, mutates local history; `<upstream> [branch]`. | Blocks the rebase. |
| `pre-merge-commit` | Immediately before an automatic merge commit, including `merge --continue`; no arguments. It does not run for fast-forward, squash, or a pending `--no-commit` result. | Blocks the merge commit. |
| `post-merge` | After a completed merge; `<squash-flag>`, where `1` means squash and `0` means a normal merge/fast-forward. It does not run for already-up-to-date or conflicted outcomes. | Advisory. |
| `post-rewrite` | After amend or completed rebase; argument `amend` or `rebase`, with `<old-oid> <new-oid>` lines on stdin. | Advisory. |

A blocking hook's launch failure, timeout, or non-zero exit aborts before the
associated ref/history mutation. Post-operation hooks observe an already
completed mutation, so their failures are warnings and never claim rollback.
Hook stdout/stderr is replayed in human mode; quiet, `--json`, and `--machine`
suppress it to preserve their output contract. With `--exit-code-on-warning`, an
advisory failure returns exit 9 even though the documented mutation remains
complete.

## Escape valves

- `libra commit --no-verify` skips all repository hooks for that commit;
  `--disable-pre` skips only `pre-commit`.
- `libra merge --no-verify` skips all hooks for that merge, including message
  hooks, `post-commit`, and a pending `pre-merge-commit` during `--continue`.
- `LIBRA_NO_HOOKS=1` skips repository hooks for commands without a dedicated
  flag, including checkout, switch, rebase, and pull. The values `true`, `yes`,
  and `on` are also accepted.

These controls bypass repository policy. Prefer fixing the hook or sandbox
configuration when possible.

## Examples

```sh
cat >.libra/hooks/pre-commit <<'EOF'
#!/bin/sh
cargo test --quiet
EOF
chmod +x .libra/hooks/pre-commit

# Review the impact before bypassing repository policy.
LIBRA_NO_HOOKS=1 libra rebase main
```
