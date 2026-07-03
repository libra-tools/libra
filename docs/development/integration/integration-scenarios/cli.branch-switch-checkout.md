### `cli.branch-switch-checkout`

目的：覆盖 `branch`、`switch`、`checkout` 的分支创建、切换、`switch -C`、`switch --orphan`、`--track` 远端分支、`branch -r`/`-a` 远端列出、detached HEAD（`switch --detach` 与 `checkout --detach`）、兼容 alias、分支重命名/删除和路径恢复行为，覆盖 `switch --guess`/`--no-guess` 的 DWIM 远端跟踪猜测，并保留 `switch -f` 未公开的负向检查。

最小步骤：

```bash
# Converged short form.
SCENARIO="cli.branch-switch-checkout"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

libra init branch-repo
cd branch-repo

libra config set user.name "Libra Branch Test"
libra config set user.email "branch@example.invalid"
printf 'base\n' > base.txt
libra add base.txt
libra commit -m "test: branch base"

libra branch --show-current
libra branch feature/cli-smoke
libra branch feature/from-main main
libra branch --list
libra switch feature/cli-smoke
printf 'feature\n' > feature.txt
libra add feature.txt
libra commit -m "test: feature branch"
libra checkout main
libra checkout -b compat-checkout
libra checkout main
libra switch -c switch-created main
libra switch main

libra switch -C reset-feature main
libra switch main

! libra switch -f force-target

libra switch --orphan orphan-root
! libra rev-parse HEAD
test ! -e tracked.txt
libra switch main

# Remote tracking uses a local remote fixture in the runner
# (the fixture carries two remote-only branches: guessed / guessed-two).
libra remote add origin "$RUN_ROOT/repos/cli.branch-switch-checkout/guess-remote"
libra fetch origin
libra branch -r
libra branch -a
libra switch --track origin/guessed
libra branch --show-current
libra switch main
# --no-guess keeps the remote-only name unresolved (must fail);
# default-on guess / explicit --guess DWIM-creates a local tracking branch.
! libra switch --no-guess guessed-two
libra switch --guess guessed-two
libra branch --show-current
libra switch main

BASE_COMMIT="$(libra rev-parse HEAD)"
libra switch --detach "$BASE_COMMIT"
! libra symbolic-ref HEAD
libra switch main

libra branch -m feature/from-main feature/renamed
libra branch -d feature/renamed
libra branch -D feature/cli-smoke

printf 'dirty\n' > base.txt
libra checkout -- base.txt
grep 'base' base.txt
libra branch

# Verify branch list JSON output
libra --json branch --list >branch-list.json
python3 -c "import json; d=json.load(open('branch-list.json')); assert d['ok'] is True; assert isinstance(d['data'].get('branches'), list)"
```

负向步骤：

```bash
cd "$RUN_DIR/branch-repo"
! libra branch "bad branch"
! libra switch no-such-branch
! libra checkout no-such-branch
! libra branch -d no-such-branch
```

断言：`branch --show-current` 输出当前分支；从 HEAD 和指定 base 创建分支成功；`switch` / `checkout` 都能切换到已存在分支；`switch -C` 能创建或重置目标分支；`switch -f` 当前未公开，必须非 0 退出；`switch --orphan` 进入 unborn branch、`rev-parse HEAD` 失败且原 tracked 文件从 index/worktree 移除；本地 remote fetch 后 `branch -r` 列出 `origin/<branch>`、`branch -a` 同时列出本地与远端分支；`switch --track origin/guessed` 能显式创建 tracking 分支；`switch --no-guess <remote-only>` 仍要求本地分支，必须非 0 退出，`switch --guess <remote-only>` 则 DWIM 创建本地跟踪分支并切换；`checkout -b` 与 `switch -c` 都能创建并切换分支；`switch --detach <commit>` 后 `symbolic-ref HEAD` 必须失败（HEAD is not a symbolic ref）；`branch -m` 后旧名消失、新名可通过 `branch --list` 列出且 `--json branch --list` 含 branches 数组；安全删除已合并分支成功且列表不再包含该分支，强制删除未合并分支成功；`checkout -- <path>` 能恢复工作区文件；非法分支名、缺失分支或缺失删除目标必须非 0 退出并保留现有分支状态。

补充可执行断言：
- 关键分支操作后 `libra --json branch --list` 解析验证新分支出现。
- detached 后 `libra symbolic-ref HEAD` 必须失败（或输出 "HEAD" 且非 ref），这是 Libra/Git 符号引用限制的验证点。
- `libra --json switch main` 成功后验证 `ok:true`。
- `switch -f` 必须保持未公开负向行为；`switch --orphan` 后 `tracked.txt` 不存在。
- `--track` 远端分支后 `branch --show-current` 必须输出 guessed；`--no-guess <remote-only>` 必须非 0 退出，`--guess <remote-only>` 成功后 `branch --show-current` 输出 guessed-two。
- 所有分支操作后 `libra fsck` 通过；删除分支后 `libra --json show-ref --heads` 的 `data.entries[]` 不再包含已删分支。
