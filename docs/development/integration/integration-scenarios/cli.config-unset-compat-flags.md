### `cli.config-unset-compat-flags`

目的：覆盖 `unset --all` 子命令参数，以及 Git 兼容隐藏 flag `--unset`、`--unset-all`。

最小步骤：

```bash
SCENARIO="cli.config-unset-compat-flags"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"
# (prelude provides libra() -- converged short form, long wrapper removed)

libra init config-repo
cd config-repo

libra config set temp.single value
libra config --unset temp.single
! libra config get temp.single

libra config set --add temp.multi one
libra config set --add temp.multi two
libra config unset --all temp.multi
! libra config get --all temp.multi

libra config set --add temp.legacy one
libra config set --add temp.legacy two
libra config --unset-all temp.legacy
! libra config --get-all temp.legacy
```

断言：单值 unset 和 all unset 都能通过后续 get 观察到删除效果；legacy hidden flags 直接 invocation 可用，但不要求 help 展示。

补充可执行断言：
- `libra --json config set temp.single value && libra --json config --unset temp.single` 后 `libra --json config get temp.single` 必须非 0 或 data 为空。
- 多值场景：`--unset-all` 后 `--json get --all` 返回空列表。
- 验证 legacy `--unset-all` 与现代 `unset --all` 行为等价。
- 操作全程使用隔离 global DB。

