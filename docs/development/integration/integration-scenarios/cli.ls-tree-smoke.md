### `cli.ls-tree-smoke`

目的：覆盖公开的 `ls-tree` Git 兼容 plumbing 命令，验证 commit/tree 内容可读、目录过滤递归、子目录输出、`--full-name`、`--full-tree`、常见输出参数、JSON envelope、负向路径错误和仓库健康。

最小步骤：

```bash
SCENARIO="cli.ls-tree-smoke"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"
# Short converged (prelude)
libra init ls-tree-repo
cd ls-tree-repo
libra config user.name "Libra Integration"
libra config user.email integration@example.invalid
printf 'base\n' >tracked.txt
libra add tracked.txt
libra commit -m "initial" --no-verify
mkdir -p src/nested
printf 'root\n' >README.md
printf 'lib\n' >src/lib.rs
printf 'deep\n' >src/nested/deep.txt
test -f src/nested/deep.txt
libra add README.md src/lib.rs src/nested/deep.txt
libra commit -m "test: ls-tree fixture" --no-verify

libra ls-tree HEAD
libra ls-tree -r HEAD src
libra ls-tree --name-only HEAD
cd src
libra ls-tree HEAD
libra ls-tree --full-name HEAD
libra ls-tree --full-tree HEAD
cd ..
libra --json ls-tree -r HEAD src >ls-tree.json
libra fsck --connectivity-only
```

负向步骤：

```bash
cd "$RUN_DIR/ls-tree-repo"
! libra ls-tree HEAD missing
```

断言：默认输出包含 root tree 的 `README.md` 与 `src`；`-r HEAD src` 输出 `src/lib.rs` 和 `src/nested/deep.txt`；`--name-only HEAD` 输出路径列表；从 `src/` 执行的默认输出包含 `lib.rs` 和 `nested` 且不包含 `README.md` / `src/lib.rs`；从 `src/` 执行 `--full-name` 输出 `src/lib.rs`；从 `src/` 执行 `--full-tree` 输出 root tree 的 `README.md` 与 `src`；`--json` 返回 `ok:true` 且命令名为 `ls-tree`；缺失路径必须非 0 退出并报告可诊断错误；场景结束后 `libra fsck --connectivity-only` 通过。
