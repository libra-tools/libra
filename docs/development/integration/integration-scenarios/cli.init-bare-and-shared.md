### `cli.init-bare-and-shared`

目的：覆盖 `--bare` 与 `--shared=<MODE>`。

最小步骤：

```bash
# Short converged form.
SCENARIO="cli.init-bare-and-shared"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

libra init --bare bare-repo
test -f bare-repo/libra.db
test -d bare-repo/objects
test ! -e bare-repo/.libra
cd bare-repo
! libra status

cd "$RUN_DIR"
libra init --shared=false shared-false
libra init --shared=true shared-true
libra init --shared=umask shared-umask
libra init --shared=group shared-group
libra init --shared=all shared-all
libra init --shared=world shared-world
libra init --shared=everybody shared-everybody
libra init --shared=0770 shared-octal

# 无值 --shared 形式（require_equals）默认折算为 group；尾词是 DIRECTORY 位置参数
libra init --shared shared-default
cd shared-default
libra config get core.sharedRepository | grep group
libra fsck --connectivity-only
cd "$RUN_DIR"
```

负向步骤：

```bash
cd "$RUN_DIR"
! libra init --shared=invalid shared-invalid
! libra init --shared=8888 shared-bad-octal
```

断言：bare 仓库把 `libra.db` 和 `objects` 放在目标目录本身，不创建普通工作区 `.libra/`；普通工作区命令在 bare 仓库中应按当前 CLI 语义失败或提示不适用；所有支持的 shared mode 退出码为 0；非法 shared mode 非 0 退出并列出支持值。Unix 平台可补充检查 shared 仓库文件权限；跨平台默认只要求 CLI 可观察仓库状态正确。

补充可执行断言：
- bare repo 后 `test -f bare-repo/libra.db && test ! -e bare-repo/.libra`。
- 在 bare repo 中 `libra status` 必须非 0。
- 所有合法 `--shared=<mode>` 模式创建后，仓库中的普通命令（如 `libra status`）成功，证明 schema 建链即可用。
- 非法 `--shared=<mode>` 值（invalid、8888）的错误必须非 0，且 stderr 列出支持的 mode。
- 操作后在 shared 仓库执行 `libra fsck --connectivity-only` 通过。
