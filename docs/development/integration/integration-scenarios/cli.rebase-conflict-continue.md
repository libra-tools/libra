### `cli.rebase-conflict-continue`

目的：覆盖 `rebase` 产生冲突后的 `--continue` / `--abort` / `--skip` 成功路径。

范围说明：P1-07a 的 `--autostash`、可重复 `--exec`、`--update-refs`、`--fork-point` 需要多组独立 history/worktree/sandbox fixture，由离线 Cargo target `compat_noninteractive_history_controls` 负责，不在本 shell scenario 重复构造；本场景继续守住通用 conflict marker 与 continue 状态机。

最小步骤：

```bash
SCENARIO="cli.rebase-conflict-continue"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"
# (prelude provides libra() -- converged short form, long wrapper removed)

libra init rebase-conflict-repo
cd rebase-conflict-repo
libra config set user.name "Libra Rebase Conflict Test"
libra config set user.email "rebase-conflict@example.invalid"

printf 'base line\n' > shared.txt
libra add shared.txt
libra commit -m "test: rebase conflict base"

libra branch topic
libra switch topic
printf 'topic change\n' > shared.txt
libra add shared.txt
libra commit -m "test: topic change"

libra switch main
printf 'main change\n' > shared.txt
libra add shared.txt
libra commit -m "test: main change"

libra switch topic
set +e
libra rebase main >rebase-conflict.out 2>rebase-conflict.err
REBASE_STATUS=$?
set -e
test "$REBASE_STATUS" -ne 0
grep '<<<<<<<' shared.txt
printf 'resolved rebase\n' > shared.txt
libra add shared.txt
libra rebase --continue
libra log --oneline -n 1
grep 'resolved rebase' shared.txt
```

负向步骤：

```bash
cd "$RUN_DIR/rebase-conflict-repo"
! libra rebase --continue
! libra rebase --abort
```

断言：`rebase main` 在 topic 提交与 main 修改同一文件时产生冲突，工作区出现冲突标记；解决冲突并 `add` 后 `rebase --continue` 成功完成重放；重放后 `log` 可见 topic 提交在 main 之上，`shared.txt` 内容为解决后的文本；无 rebase 会话时 `rebase --continue` / `rebase --abort` 必须失败且不破坏当前分支状态。

补充可执行断言（rebase 冲突）：
- 冲突后 `libra --json status` 可解析 `data.merge_state.conflicted_paths[]`，且包含 `shared.txt`。
- `rebase --continue` 成功后 `libra --json status` 显示 `data.is_clean == true`，且 `data.merge_state` 缺失或 `conflicted_paths` 为空。
- `libra fsck` 在 rebase --continue/--abort 后通过。
- 负向 rebase --continue 无会话错误必须可识别（stderr 捕获验证 "rebase" 或 LBR-）。
