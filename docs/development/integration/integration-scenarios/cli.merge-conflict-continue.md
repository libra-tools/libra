### `cli.merge-conflict-continue`

目的：覆盖 `merge` 产生冲突后的 `--continue` / `--abort` 成功路径。

最小步骤：

```bash
SCENARIO="cli.merge-conflict-continue"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"
# (prelude provides libra() -- converged short form, long wrapper removed)

libra init merge-conflict-repo
cd merge-conflict-repo
libra config set user.name "Libra Merge Conflict Test"
libra config set user.email "merge-conflict@example.invalid"

printf 'base line\n' > shared.txt
libra add shared.txt
libra commit -m "test: merge conflict base"

libra branch side
libra switch side
printf 'side change\n' > shared.txt
libra add shared.txt
libra commit -m "test: side change"

libra switch main
printf 'main change\n' > shared.txt
libra add shared.txt
libra commit -m "test: main change"

set +e
libra merge side >merge-conflict.out 2>merge-conflict.err
MERGE_STATUS=$?
set -e
test "$MERGE_STATUS" -ne 0
grep '<<<<<<<' shared.txt
printf 'resolved merge\n' > shared.txt
libra add shared.txt
libra merge --continue
libra log --oneline -n 1
grep 'resolved merge' shared.txt
```

负向步骤：

```bash
cd "$RUN_DIR/merge-conflict-repo"
! libra merge --continue
! libra merge --abort
```

断言：`merge side` 在同一文件产生冲突，工作区出现冲突标记；解决冲突并 `add` 后 `merge --continue` 成功完成合并提交；合并后 `log` 可见 merge commit，`shared.txt` 内容为解决后的文本；无 merge 会话时 `merge --continue` / `merge --abort` 必须失败且不破坏当前分支状态。

P1-07b 的参数矩阵由同 Wave 1 的 Cargo target `compat_noninteractive_history_controls` 承担，避免 shell runner 重复构造多组历史：`-s ours` 固定双父与 current-tree；`-X ours/theirs` 固定同文件 clean/conflict hunk 分离；`--allow-unrelated-histories` 覆盖默认拒绝、clean root 合并和 conflict→restart→continue；`--log[=<n>]`/`--no-log` 覆盖 last-wins 与自定义消息跨 continue。

补充可执行断言（冲突场景核心）：
- 冲突后 `libra --json status` 必须显示 `data.merge_state.conflicted_paths[]` 非空。
- `merge --continue` 成功后 `libra --json status` 显示 `data.is_clean == true`，且 `data.merge_state` 缺失或 `conflicted_paths` 为空。
- `libra fsck` 在 continue/abort 后必须通过。
- 负向 continue/abort 的错误必须是可识别的 "no merge in progress" 类（捕获 stderr 验证包含 "merge" 或 LBR-CONFLICT 相关）。
