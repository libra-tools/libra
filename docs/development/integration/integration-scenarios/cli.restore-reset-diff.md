### `cli.restore-reset-diff`

目的：覆盖当前 `diff`、`restore`、`reset` 已暴露参数的端到端行为。`reset --merge/--keep` 在本场景验证公开正向入口；本地变更保留、覆盖拒绝和失败回滚的精确矩阵由 `compat_noninteractive_history_controls` 守卫。

最小步骤：

```bash
SCENARIO="cli.restore-reset-diff"
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

printf 'modified\n' > tracked.txt
libra diff
libra diff tracked.txt
libra diff --name-only
libra diff --stat
libra diff --numstat
libra diff --raw -z
libra diff --name-status --diff-filter=M
libra diff --compact-summary
libra add tracked.txt
libra diff --staged
libra diff --staged --name-status
libra diff --staged --full-index --src-prefix=old/ --dst-prefix=new/
libra restore --staged tracked.txt
libra restore --worktree tracked.txt

printf 'second\n' > tracked.txt
libra add tracked.txt
libra reset tracked.txt
libra add tracked.txt
libra commit -m "second" --no-verify
libra diff --old HEAD~1 --new HEAD --numstat
printf 'source probe\n' > tracked.txt
libra restore --source HEAD~1 tracked.txt
libra --json restore --source HEAD --no-overlay tracked.txt
libra --json restore --source HEAD --overlay tracked.txt
libra reset --hard HEAD
libra reset --soft HEAD~1
libra reset --mixed HEAD
libra reset --hard HEAD
libra reset --merge HEAD
libra reset --keep HEAD

printf 'diff output probe\n' > tracked.txt
libra --json diff
libra diff --output diff-out.patch tracked.txt
libra diff --algorithm=histogram tracked.txt
libra fsck --connectivity-only
```

负向步骤：

```bash
! libra --json restore --pathspec-from-file=restore-paths.txt
! libra --json reset --pathspec-from-file=reset-paths.txt
! libra diff --algorithm myers tracked.txt
! libra diff --old no-such-revision --new HEAD
! libra restore nonexistent.txt
! libra restore --source no-such-revision tracked.txt
! libra reset --hard no-such-rev
```

关键断言：

- `diff` 在 unstaged、staged、revision-to-revision、文件输出和 JSON 输出路径中返回可观察差异；`--raw -z` 提供 NUL-safe mode/object/status 记录，`--diff-filter=M` 只保留修改项，`--compact-summary` 进入 stat 表面，`--full-index` 与显式 src/dst 前缀改写 patch header。
- `restore --staged` 取消暂存，`restore --worktree` 和 `restore --source` 恢复工作区内容。
- `reset <path>` / `reset HEAD -- <path>` 只取消暂存；revision/path 同名时必须用 `--` 消歧；`--soft`、`--mixed`、`--hard` 覆盖基础 HEAD/index/worktree 行为；`--merge`、`--keep` 的 no-op target 正向入口成功。
- 缺失 pathspec 文件、无效 diff algorithm 和不存在的 revision/path 必须返回稳定错误，且错误路径不能移动 HEAD 或改写目标文件。
- 场景结束时运行 `fsck --connectivity-only`。
