### `cli.clone-fetch-pull-local`

目的：验证当前本地路径 Git remote 的 clone、remote、ls-remote、fetch、pull 基础互操作，并用负向断言记录仍未实现的 Git 参数。

最小步骤：

```bash
SCENARIO="cli.clone-fetch-pull-local"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

# fixture 由 runner 通过 gitfix() 创建本地 Git 仓库。
libra clone "$REMOTE_DIR" clone
cd clone
grep first README.md
libra remote -v
libra remote show origin
libra remote get-url origin
libra remote set-url origin "$REMOTE_DIR"
libra remote add backup "$REMOTE_DIR"
libra remote rename backup backup-renamed
libra remote remove backup-renamed
libra remote prune origin

libra ls-remote --heads origin
libra ls-remote --tags origin
libra ls-remote --refs origin
libra ls-remote --get-url origin
libra ls-remote --sort=version:refname --tags origin
libra ls-remote --exit-code origin main
libra ls-remote --symref origin  # local Git remote (git-upload-pack) advertises symref=HEAD → prints `ref: refs/heads/main\tHEAD` above HEAD's OID line
! libra ls-remote --exit-code origin no-match  # exit 2, silent
! libra remote set-branches origin main
! libra remote set-head origin main
! libra remote update origin

libra fetch origin
libra fetch --all
libra fetch origin --depth 2
! libra fetch --deepen 1 origin
! libra fetch --unshallow origin
! libra fetch --prune origin

libra pull
libra pull --ff-only
libra pull --rebase
libra pull --squash
libra pull --commit
libra pull --autostash
libra fsck --connectivity-only
```

关键断言：

- 本地 Git fixture 可被 Libra clone，工作区内容、remote config 和 refs 可观察。
- 当前 `remote`、`ls-remote`、`fetch`、`pull` 支持面覆盖基础正向路径，其中 `ls-remote --get-url` 不做 discovery，`--sort=version:refname` 按 refname 版本顺序排序，`--exit-code` 在无匹配时返回 2。
- `remote set-branches/set-head/update`、`fetch --deepen/--unshallow/--prune` 当前作为负向路径验证；`pull --squash`/`--commit`/`--autostash` 已实现并作为正向路径验证。
- 失败路径不得破坏 clone 仓库，结尾 `fsck --connectivity-only` 必须通过。
