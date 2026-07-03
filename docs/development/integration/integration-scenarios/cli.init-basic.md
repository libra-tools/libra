### `cli.init-basic`

目的：验证 `libra init` 的默认初始化路径创建可用普通仓库，并作为所有 init 参数矩阵的最小基线。

最小步骤：

```bash
# Short converged form.
SCENARIO="cli.init-basic"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

libra init repo
test -f repo/.libra/libra.db
test -d repo/.libra/objects
cd repo
libra status
libra fsck --connectivity-only

# DIRECTORY 位置参数默认值 "."：在目标目录内不带参数执行 init
mkdir -p "$RUN_DIR/default-dir"
cd "$RUN_DIR/default-dir"
libra init
test -f .libra/libra.db
```

负向步骤：

```bash
cd "$RUN_DIR"
! libra init repo
```

断言：初始化命令退出码为 0；`.libra/libra.db` 和对象目录存在；`status`、`fsck --connectivity-only` 可在新仓库中执行（普通命令建链即把 schema 升级到当前版本）；重复初始化同一路径必须非 0 或明确提示已有仓库，且不得破坏既有 `.libra` 布局。

补充可执行断言：
- `libra --json status` 必须返回 `ok:true`，且 `data.head.type == "branch"`、`data.head.name` 指向初始分支。
- 重复 init 失败时 stderr 必须包含已有仓库/目标路径相关错误或 LBR- 稳定码。

