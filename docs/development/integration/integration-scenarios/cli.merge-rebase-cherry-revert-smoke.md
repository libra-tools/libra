### `cli.merge-rebase-cherry-revert-smoke`

目的：覆盖当前 history-edit 命令的本地成功路径，并用负向断言记录未支持或受限的 Git 参数。

P1-10 的 required-sandbox `pre-merge-commit` / merge message hooks / `pre-rebase`、
advisory `post-commit` / `post-merge` / `post-rewrite`、rewrite stdin、
`--no-verify`/环境逃逸阀，以及
blocking failure 和 hook-created untracked collision 的 ref 原子性由注册的 Wave 1
Cargo target `compat_libra_hooks_lifecycle` 负责。

最小步骤：

```bash
SCENARIO="cli.merge-rebase-cherry-revert-smoke"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

libra init repo
cd repo
libra config user.name "Libra Integration"
libra config user.email "integration@example.invalid"
printf 'base\n' > tracked.txt
libra add tracked.txt
libra commit -m "initial" --no-verify

libra switch -c feature
printf 'feature\n' > feature.txt
libra add feature.txt
libra commit -m "feature" --no-verify
libra switch main
printf 'main\n' > main.txt
libra add main.txt
libra commit -m "main work" --no-verify
libra merge feature

libra switch -c topic
printf 'topic\n' > topic.txt
libra add topic.txt
libra commit -m "topic" --no-verify
TOPIC="$(libra rev-parse HEAD)"
libra switch main
libra cherry-pick "$TOPIC"
libra show --no-patch HEAD
libra revert HEAD
libra cherry-pick -x "$TOPIC"
libra show --no-patch HEAD
libra revert HEAD
libra --json log --oneline

! libra --json revert HEAD~2..HEAD
! libra revert --continue
! libra revert --abort
! libra merge --find-renames=90 rename-side
! libra merge --abort
! libra merge --squash --continue
! libra merge nonexistent-branch
libra fsck --connectivity-only
```

关键断言：

- `merge`、默认 `cherry-pick`、`cherry-pick -x`、单提交 `revert` 和 JSON `log` 当前可用。
- 默认 `cherry-pick` 的提交消息不应追加来源行；`cherry-pick -x` 的提交消息应追加 `(cherry picked from commit <TOPIC>)`。
- revert range、空会话 lifecycle、`merge --find-renames`、`merge --squash` 和缺失分支必须失败。
- criss-cross fixture 当前断言 `--json rebase right` 成功，并在完成后运行 `fsck --connectivity-only`。
