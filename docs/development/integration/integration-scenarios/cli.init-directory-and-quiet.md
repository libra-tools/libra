### `cli.init-directory-and-quiet`

目的：覆盖位置参数 `DIRECTORY`、短参数 `-q` 和长参数 `--quiet`。

最小步骤：

```bash
# Converged short form: prelude once at top.
SCENARIO="cli.init-directory-and-quiet"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

libra init nested/repo
test -f nested/repo/.libra/libra.db
test -d nested/repo/.libra/objects
cd nested/repo
libra status

cd "$RUN_DIR"
libra init -q quiet-short >quiet-short.out 2>quiet-short.err
libra init --quiet quiet-long >quiet-long.out 2>quiet-long.err
test -f quiet-short/.libra/libra.db
test -f quiet-long/.libra/libra.db
```

断言：`DIRECTORY` 可创建不存在的嵌套目录；`-q` / `--quiet` 退出码为 0；quiet 模式不输出普通初始化 banner，但错误仍应写入 stderr；quiet 仓库进入目录后 `status` 可执行。

补充可执行断言：
- `libra --json init -q quiet-json-repo` 成功（ok:true），且 `test -f quiet-json-repo/.libra/libra.db`。
- quiet 模式下 stdout 为空（或极小），但 stderr 可包含初始化信息。
- 操作后 `libra fsck --connectivity-only` 在 quiet 仓库中通过。
- 所有 init 使用隔离 LIBRA_CONFIG_GLOBAL_DB，结束后验证无全局污染。

