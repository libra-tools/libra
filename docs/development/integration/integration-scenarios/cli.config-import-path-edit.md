### `cli.config-import-path-edit`

目的：覆盖 `import`、`path`、`edit` 子命令，以及 Git 兼容隐藏 flag `--import`。

最小步骤：

```bash
SCENARIO="cli.config-import-path-edit"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"
# (prelude provides gitfix/libra for this requires_git scenario; dupe resolution removed for convergence)

mkdir git-config-source
cd git-config-source
gitfix init
gitfix config user.name "Imported Git User"
gitfix config user.email "imported@example.invalid"

libra init libra-import-target
cd libra-import-target
libra config import
libra config get user.name
libra config get user.email
libra config path

cd "$RUN_DIR/git-config-source"
libra init libra-import-legacy
cd libra-import-legacy
libra config --import

! libra config edit
```

断言：`config import` / `--import` 从 Git config 导入当前 scope 可接受的配置项，不接受任意文件路径作为参数；`path` 输出当前 scope 的 config DB 路径且路径存在；`edit` 当前因 SQLite 存储不支持文本编辑，必须非 0 退出并提示使用 `set/unset/list`。

补充可执行断言：
- `libra --json config path` 成功且 data.path 指向 .libra/libra.db 或 global DB。
- `libra --json config import` 成功后，`libra --json config get user.name` 返回从 Git fixture 导入的值。
- `! libra config edit` 必须非 0，stderr 包含 "set/unset/list" 或等价提示。
- 验证 import 只导入当前 scope 可接受的 key。

