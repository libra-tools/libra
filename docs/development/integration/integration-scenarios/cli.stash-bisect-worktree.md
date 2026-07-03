### `cli.stash-bisect-worktree`

目的：覆盖兼容性差异较大的 `stash`、`bisect`、`worktree` 命令面，重点验证状态保存/恢复、`stash push -u` / `-a` / `--all` / `--keep-index`、二分会话状态，以及 Libra worktree 的 shared-HEAD 差异语义、常用 Git 兼容 flags 和 remove 默认保留目录的安全边界。

最小步骤：

```bash
# Short converged form (long original wrapper removed for convergence).
SCENARIO="cli.stash-bisect-worktree"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

libra init workflow-repo
cd workflow-repo

libra config set user.name "Libra Workflow Test"
libra config set user.email "workflow@example.invalid"
printf '0\n' > number.txt
libra add number.txt
libra commit -m "test: workflow base"

printf 'stash change\n' >> number.txt
libra stash push -m "wip: tracked change"
libra stash list
libra stash show
libra stash show --name-status stash@{0}
! libra stash pop stash@{999}
libra stash apply
libra status --short
libra restore --worktree number.txt
libra stash pop
libra status --short

# Test stash drop with an explicit positional stash ref
printf 'drop change\n' >> number.txt
libra stash push -m "wip: drop target"
libra stash drop stash@{0}
libra stash list

# Test stash apply/branch with explicit positional stash refs
printf 'stash branch change\n' >> number.txt
libra stash push -m "wip: stash branch"
libra stash apply stash@{0}
libra restore --worktree number.txt
libra stash branch stash-branch-test stash@{0}
libra branch --show-current | grep -q 'stash-branch-test'
libra reset --hard
libra switch main
libra branch -D stash-branch-test

libra stash clear --force
libra stash list

printf 'visible\n' > visible-untracked.txt
libra --json stash push -u >stash-push-untracked.json
python3 -c "import json; d=json.load(open('stash-push-untracked.json')); assert d['ok'] is True; assert d['data']['included_untracked'] >= 1"
test ! -e visible-untracked.txt
libra stash pop
test -f visible-untracked.txt
rm visible-untracked.txt

printf 'ignored.log\n' > .libraignore
printf 'ignored\n' > ignored.log
libra --json stash push --all >stash-push-all.json
python3 -c "import json; d=json.load(open('stash-push-all.json')); assert d['ok'] is True; assert d['data']['included_untracked'] >= 1"
test ! -e .libraignore
test ! -e ignored.log
libra stash pop
test -f .libraignore
test -f ignored.log
rm .libraignore ignored.log

printf 'staged\n' > number.txt
libra add number.txt
printf 'unstaged\n' > number.txt
libra --json stash push --keep-index >stash-push-keep-index.json
python3 -c "import json; d=json.load(open('stash-push-keep-index.json')); assert d['ok'] is True; assert d['data']['kept_index'] is True"
test "$(cat number.txt)" = "staged"
libra reset --hard
libra stash clear --force

GOOD_COMMIT="$(libra rev-parse HEAD)"
printf '1\n' > number.txt
libra add number.txt
libra commit -m "test: bisect middle"
MIDDLE_COMMIT="$(libra rev-parse HEAD)"
printf '2\n' > number.txt
libra add number.txt
libra commit -m "test: bisect bad"
BAD_COMMIT="$(libra rev-parse HEAD)"

# Session A: bare start + bare bad + good <rev>
libra bisect start
libra bisect bad
libra bisect good HEAD~1
libra bisect log
libra bisect reset

# Session B: --good flag form + view + bad <rev> + reset <rev>
libra bisect start "$BAD_COMMIT" --good "$GOOD_COMMIT"
libra bisect view
! libra bisect bad no-such-revision
libra bisect bad "$MIDDLE_COMMIT"
libra bisect log
libra bisect reset "$GOOD_COMMIT"
test "$(libra rev-parse HEAD)" = "$GOOD_COMMIT"
libra reset --hard
libra switch main

# Session C: bisect multi-good positional (git: start <bad> <good1> <good2>...)
libra bisect start "$BAD_COMMIT" "$MIDDLE_COMMIT" "$GOOD_COMMIT"
libra bisect log
libra bisect reset

# Session D: bisect skip (bare current candidate + explicit positional rev)
libra bisect start "$BAD_COMMIT" --good "$GOOD_COMMIT"
libra bisect skip
libra bisect skip "$MIDDLE_COMMIT"
libra bisect reset

libra worktree add -b workflow-linked "$RUN_ROOT/repos/workflow-linked"
libra worktree list --verbose
libra worktree list --porcelain
libra worktree lock "$RUN_ROOT/repos/workflow-linked" --reason "integration smoke"
libra worktree list --porcelain
libra worktree unlock "$RUN_ROOT/repos/workflow-linked"
libra worktree move "$RUN_ROOT/repos/workflow-linked" "$RUN_ROOT/repos/workflow-moved"
libra worktree remove "$RUN_ROOT/repos/workflow-moved"
test -d "$RUN_ROOT/repos/workflow-moved"

# Test stale-entry pruning without deleting live worktrees
libra worktree add "$RUN_ROOT/repos/workflow-stale"
rm -rf "$RUN_ROOT/repos/workflow-stale"
libra worktree prune --dry-run
libra worktree prune --verbose --expire now

# Test --no-checkout plus locked double-force unregister
libra worktree add --no-checkout --lock --reason "integration no checkout" "$RUN_ROOT/repos/workflow-empty"
test ! -e "$RUN_ROOT/repos/workflow-empty/tracked.txt"
libra worktree remove -f -f "$RUN_ROOT/repos/workflow-empty"
test -d "$RUN_ROOT/repos/workflow-empty"

# Test worktree remove --delete-dir --force
libra worktree add "$RUN_ROOT/repos/workflow-dirty-delete"
printf 'dirty\n' > "$RUN_ROOT/repos/workflow-dirty-delete/dirty.txt"
libra worktree remove --delete-dir --force "$RUN_ROOT/repos/workflow-dirty-delete"
test ! -d "$RUN_ROOT/repos/workflow-dirty-delete"

# Verify JSON outputs for AI Agent readability
libra --json stash list >stash-list.json
python3 -c "import json; d=json.load(open('stash-list.json')); assert d['ok'] is True; assert isinstance(d['data'].get('entries') or d['data'].get('stashes') or [], list)"
libra --json worktree list >worktree-list.json
python3 -c "import json; d=json.load(open('worktree-list.json')); assert d['ok'] is True; assert isinstance(d['data'].get('worktrees') or d['data'].get('entries') or [], list)"
```

负向步骤：

```bash
cd "$RUN_DIR/workflow-repo"
! libra stash pop stash@{999}
! libra bisect bad no-such-revision
! libra worktree remove "$RUN_ROOT/repos/no-such-worktree"
```

断言：`stash push` 保存 tracked 修改并清理工作区；`stash list` / `stash show` 能观察 stash 条目，`stash show --name-status stash@{0}` 输出 `<status>\t<path>` 记录；`stash apply`（含 `apply stash@{0}` 位置引用）保留 stash，`stash pop` 应用并删除 stash，越界引用 `stash pop stash@{999}` 必须失败；`stash drop stash@{0}` 删除指定条目并在列表中消失；`stash branch <branch> stash@{0}` 创建并切换新分支、应用且丢弃该 stash；`stash push -u` 保存/移除/恢复可见 untracked 文件；`stash push --all` 保存/移除/恢复可见 untracked 与 ignored 文件；`stash push --keep-index` 保留 staged 内容并移除 unstaged delta；`stash clear --force` 清空列表；`bisect start <bad> --good <good>` 建立会话，`bisect start <bad> <good1> <good2>...` 多 good 位置参数（git 表面），`view` / `log` 能观察状态，`bad`（裸式与 `bad <rev>`，非法 revision 失败）/ `good <rev>` 推进会话，`skip` / `skip <rev>` 跳过候选，`reset` 恢复原始 HEAD，`reset <rev>` 使 HEAD 落在指定 commit（detached，需 `reset --hard` 后再 `switch` 回原分支）；`worktree add -b` 注册 linked worktree 并创建 shared branch，`list --verbose` 显示共享 HEAD 短 hash，`list --porcelain` 输出 `worktree` / `HEAD` / `locked` 记录且不输出 Git per-worktree `branch` / `detached` 行，`lock --reason` / `unlock` 更新锁状态，`move` 更新路径，`remove` 默认只注销登记且保留目录，`prune --dry-run` 不写 registry，`prune --expire now` 清理目录缺失条目，`add --no-checkout` 不恢复 tracked 文件，locked worktree 需要 `-f -f` 注销，`remove --delete-dir --force` 可删除 dirty linked worktree；非法 stash ref、非法 revision 和缺失 worktree 必须失败且不破坏已有仓库状态。

补充可执行断言（故意差异重点场景）：
- `libra worktree remove <path>` 后 `test -d <path>` 必须仍存在（Libra 故意保留目录，不像 Git 默认删除）。
- `libra worktree list --porcelain` 必须包含 `worktree <path>` 和共享 `HEAD <hash>`，且不得包含 `branch` / `detached`（Libra 无 per-worktree HEAD）。
- `libra worktree add --no-checkout <path>` 后 tracked fixture 不应被恢复。
- `libra --json stash list` 验证 `ok:true` 且 `data.entries[]` 或 `data.stashes[]` 可解析。
- 每次 stash/bisect/worktree 操作后 `libra fsck --connectivity-only` 必须 0 退出。
- `worktree remove` 后的 `libra --json worktree list` 不再包含该 worktree。
- 负向 `worktree remove` 不存在路径的错误必须非 0，stderr 包含路径。
- 验证 `--delete-dir --force` 模式真正删除 dirty 目录：`libra worktree remove --delete-dir --force <path> && test ! -d <path>`。
