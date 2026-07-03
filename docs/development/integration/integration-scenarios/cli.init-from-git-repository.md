### `cli.init-from-git-repository`

目的：覆盖 `--from-git-repository <path>`，验证本地 Git 仓库转换为 Libra 仓库的 CLI 可观察行为。

最小步骤：

```bash
# Converged: use top-level prelude which provides SAFE_PATH/gitfix/libra (handles git for requires_git scenarios).
SCENARIO="cli.init-from-git-repository"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

mkdir git-source
cd git-source
gitfix init
gitfix config user.name "Git Fixture"
gitfix config user.email "git-fixture@example.invalid"
printf 'from git\n' > README.md
gitfix add README.md
gitfix commit -m "fixture: initial"

cd "$RUN_DIR"
libra init --from-git-repository git-source converted
cd converted
libra status
libra log --oneline
test -f README.md
```

负向步骤：

```bash
cd "$RUN_DIR"
! libra init --from-git-repository missing-source converted-missing
```

断言：转换后的 Libra 仓库可执行 `status` 和 `log`；至少一个来自 Git fixture 的文件、提交或 ref 可通过 `libra` 命令观察；缺失 source 路径非 0 退出并提示有效 Git 仓库要求。这里的 Git 仓库只作为本地 fixture，不进入 GitHub live 语义。

补充可执行断言：
- 转换后 `libra --json status` 和 `libra --json log -n 1` 均 `ok:true`，且 `data.commits[]` 非空。
- `test -f converted/README.md` 且内容与 Git fixture 一致。
- 转换后的仓库 `libra fsck --connectivity-only` 通过。
- 缺失 source 时错误必须非 0，包含 "valid Git repository" 或等价提示。
- 使用 gitfix() 创建的 fixture 必须严格隔离（无主机 GIT_* 污染）。

