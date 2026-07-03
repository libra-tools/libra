### `cli.config-basic-kv`

目的：覆盖 `config set/get/list/unset` 子命令、位置参数 `key`、位置参数 `value`，以及默认 local scope。

最小步骤：

```bash
# Prelude (RUN_ROOT / SAFE_PATH / libra() / gitfix()) has been copied once at the top of this run
# (see "手动执行 prelude" above or §3.3.1). Only scenario-local steps are shown.
SCENARIO="cli.config-basic-kv"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

libra init config-repo
cd config-repo

libra config set user.name "Libra Config Test"
libra config get user.name
libra config list
libra config unset user.name
! libra config get user.name
libra config get --default fallback user.name
```

断言：`set` 后 `get` 输出设置值；`list` 包含 `user.name=Libra Config Test` 或等价 key/value 输出；`unset` 后普通 `get` 按缺失语义非 0 或无值，带 `--default` 返回 fallback。

补充可执行断言（config 家族基础模式）：
- `libra --json config get user.name` 必须返回 `ok:true`，且 `data.value == "Libra Config Test"`。
- `libra --json config list` 解析验证 `data.entries[]` 或等价结构包含本场景设置的 key。
- unset 后 `libra config get --default fallback user.name` 必须输出 fallback 且退出码 0。
- 整个场景操作后，用隔离 `LIBRA_CONFIG_GLOBAL_DB` 执行 `libra config --global list` 不得残留本场景的 user.name（严格隔离验证）。
- 负向 `libra config get 不存在的key` 必须非 0，可选捕获 stderr 验证错误文本或 LBR- 码。

