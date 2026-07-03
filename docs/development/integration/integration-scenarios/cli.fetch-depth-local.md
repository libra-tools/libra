### `cli.fetch-depth-local`

目的：验证当前本地 Git fixture 上 `clone --depth` 和 `fetch --depth` 的可观察 shallow 行为，并记录 deepen/unshallow 类参数尚未实现。

最小步骤：

```bash
SCENARIO="cli.fetch-depth-local"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

# fixture 由 runner 通过 gitfix() 创建三提交本地 Git 仓库。
libra clone --depth 1 "$REMOTE_DIR" depth-one
test -f depth-one/.libra/shallow
grep third depth-one/README.md

cd depth-one
libra fetch origin --depth 2
test -f .libra/shallow
! libra fetch origin --deepen 1
! libra fetch origin --unshallow
cd ..

libra clone --depth 2 "$REMOTE_DIR" depth-two
test -f depth-two/.libra/shallow
! libra clone --depth 0 "$REMOTE_DIR" bad-depth
```

关键断言：

- shallow clone 后工作区内容来自最新提交。
- `.libra/shallow` marker 存在，`fetch --depth 2` 可执行。
- illegal depth、`--deepen`、`--unshallow` 当前作为负向路径验证。
