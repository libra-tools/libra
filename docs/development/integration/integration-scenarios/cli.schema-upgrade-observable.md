### `cli.schema-upgrade-observable`

目的：验证新建仓库的 SQLite schema 可被 CLI 正常使用，且过期 schema 在建立数据库连接时自动升级（不再需要显式的 `libra db upgrade`）。

最小步骤：

```bash
SCENARIO="cli.schema-upgrade-observable"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"
# (prelude provides libra() -- converged short form, long wrapper removed)

libra init schema-repo
cd schema-repo

# 任意普通命令都会在打开数据库连接时把 schema 升级到当前版本；
# 对全新仓库这是幂等的（已是最新）。
libra status

libra config set user.name "Libra Schema Test"
libra config set user.email "schema@example.invalid"
printf 'schema\n' > schema.txt
libra add schema.txt
libra commit -m "test: schema usable after status"
libra log --oneline -n 1
libra fsck --connectivity-only
```

负向步骤：

```bash
cd "$RUN_ROOT/repos"
mkdir not-a-repo
cd not-a-repo
! libra status
```

断言：全新仓库上的普通命令（`status`/`add`/`commit`/`log`/`fsck`）全部 0 退出，证明 schema 建链即可用且自动保持最新；提交闭环与 `fsck --connectivity-only` 不触发 migration 或 schema 错误；非仓库目录中的命令必须失败并提示缺少 Libra 仓库。

补充可执行断言：
- 过期 schema 的仓库在执行任意需要数据库的命令时被自动升级（迁移在建立连接处应用，见 `db::establish_connection`），无需用户干预。
- 比当前二进制更新的 schema 无法降级，仍是硬错误，提示安装更新版本的 Libra。
- 非仓库目录执行普通命令必须非 0，stderr 包含 "not a libra repository" 或 LBR-REPO-001。
- 操作后 `libra fsck --connectivity-only` 必须 0 退出。
