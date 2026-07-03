### `cli.clean-rm-mv-lfs-basic`

目的：覆盖工作树管理剩余命令 `clean`、`rm`、`mv` 和本地确定性的 `lfs track/untrack/ls-files` 行为；远端 LFS lock/locks/unlock 与对象同步（push/fetch/prune/checkout）不进入默认 Wave，委托 `tests/command/lfs_test.rs` 的 mock-server 命令测试。

最小步骤：

```bash
SCENARIO="cli.clean-rm-mv-lfs-basic"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"
# (prelude provides libra() -- converged short form, long wrapper removed)

libra init worktree-tools-repo
cd worktree-tools-repo
libra config set user.name "Libra Worktree Tools Test"
libra config set user.email "worktree-tools@example.invalid"
mkdir -p docs assets
printf 'keep\n' > docs/keep.txt
printf 'move\n' > docs/move.txt
printf 'dry\n' > docs/dry.txt
printf 'verbose\n' > docs/verbose.txt
printf 'json\n' > docs/json.txt
printf 'skip\n' > docs/skip.txt
printf 'remove\n' > docs/remove.txt
libra add docs/keep.txt docs/move.txt docs/dry.txt docs/verbose.txt docs/json.txt docs/skip.txt docs/remove.txt
libra commit -m "test: worktree tools base"

libra mv docs/move.txt docs/moved.txt
libra mv -n docs/dry.txt docs/dry-moved.txt
test -f docs/dry.txt
test ! -e docs/dry-moved.txt
libra mv -v docs/verbose.txt docs/verbose-moved.txt
libra --json mv --sparse docs/json.txt docs/json-moved.txt
libra mv -k docs/missing-skip.txt docs/skip.txt assets
libra status --short
libra commit -a -m "test: move tracked file"

libra rm docs/remove.txt
libra status --short
libra commit -m "test: remove tracked file"

# clean 不接受 pathspec（仓库级操作）：逐级制造 untracked 文件再清理
printf 'scratch\n' > scratch.log
libra clean -n
test -f scratch.log
libra clean -f
test ! -f scratch.log
mkdir -p tmpdir
printf 'dir scratch\n' > tmpdir/dir-file.txt
libra clean -fd
test ! -e tmpdir
printf '*.ignored\n' > .libraignore
printf 'ignored\n' > scratch.ignored
libra clean -nX
test -f scratch.ignored
libra clean -fX
test ! -f scratch.ignored
test -f .libraignore

libra lfs track '*.bin'
libra lfs track
printf 'libra lfs payload\n' > assets/blob.bin
libra add .libra_attributes assets/blob.bin
libra commit -m "test: lfs tracked file"
libra lfs ls-files
libra lfs ls-files --long --size
libra lfs ls-files --name-only
libra --json lfs ls-files
libra lfs untrack '*.bin'
libra lfs track
```

负向步骤：

```bash
cd "$RUN_DIR/worktree-tools-repo"
! libra clean
! libra clean -xX
! libra rm no-such-file.txt
! libra mv no-such-source.txt docs/dest.txt
! libra lfs lock assets/blob.bin
```

断言：`mv` 同时更新工作区路径和 index 状态；`mv -n` 打印 dry-run 两行（`Checking rename` + `Renaming`）且不移动文件；`mv -v` 只打印实际 rename；`mv -k` 跳过缺失来源但移动有效来源；`libra --json mv --sparse` 返回 `ok:true` 且 `--sparse` 不进入 `MvOutput` JSON；`rm` 删除 tracked 文件并可提交；`clean` 不接受 pathspec：`clean -n` 列出候选且不删除、`clean -f` 删除 untracked 文件、`clean -fd` 删除 untracked 目录、`clean -nX`/`clean -fX` 只预览/删除 ignored 文件（`.libraignore` 自身为非 ignored untracked 文件，必须保留）；`lfs track` 写入 `.libra_attributes`，无参数打印 `Listing tracked patterns` 头并列出 pattern；tracked 大文件提交后 `lfs ls-files` 列出短 OID 行，`--long --size` 显示固定 payload 的 64 位完整 sha256 OID 与 `(18 B)` 尺寸，`--name-only` 仅输出路径；`lfs untrack` 后列表不再含 `*.bin`；缺 run-mode 的 `clean`、互斥的 `clean -xX`、缺失 rm/mv 源必须失败；`lfs lock` 在无远端 LFS 服务/认证时必须失败且不得泄露凭据。`lfs untrack` 对缺失 pattern 当前可能是幂等空删除，不作为负向断言。

补充可执行断言：
- `libra --json lfs ls-files` 返回 `ok:true`；有 tracked 文件时 `data.files[]` 必须可解析（无 LFS tracked 文件时 `data.files` 可缺失）。
- `libra --json mv --sparse` 返回 `ok:true`，`data` 字段集合不包含 `sparse`。
- `libra mv -k docs/missing-skip.txt docs/skip.txt assets` 移动 `docs/skip.txt` 到 `assets/skip.txt`，缺失来源不产生 stderr。
- 验证 `.libra_attributes` 内容包含 `*.bin`。
- `libra --json status` 在 mv/rm 后可解析。
- 操作后 `libra fsck --connectivity-only` 通过。
- 全局隔离：本场景的 `.libraignore` 和 LFS pattern 不得通过隔离 HOME 的全局 config 泄露到其他场景。
- 远端面（`lfs locks/lock/unlock/push/fetch/prune/checkout`、`rm --cached/-r/-f/--dry-run/--ignore-unmatch/--pathspec-*`、`mv -f`、`clean -i/-e/-ff/-x` 行为面）委托 `tests/command/remove_test.rs`、`tests/command/mv_test.rs`、`tests/command/clean_test.rs`、`tests/command/lfs_test.rs` 命令测试（见参数覆盖表）。
