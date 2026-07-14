### `cli.commit-status-log`

目的：覆盖 `status`、`add`、`commit`、`log` 的本地闭环，并记录 `status -z` 的 NUL 结尾输出。

P1-10 的任意命令执行面不在该 shell runner 中重复裸跑：`pre-commit`、
`prepare-commit-msg`、`commit-msg`、`post-rewrite`、`post-commit` 的顺序、消息
round-trip、advisory warning exit、sandbox fail-closed 与逃逸阀由注册的 Wave 1 Cargo
target `compat_libra_hooks_lifecycle` 负责；该 target 同时守卫 caller env secret 不被
hook 继承，以及 amend 的 `post-commit` → `post-rewrite` 顺序。

最小步骤：

```bash
SCENARIO="cli.commit-status-log"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

libra init repo
cd repo
libra config user.name "Libra Integration"
libra config user.email "integration@example.invalid"
printf 'hello\n' > tracked.txt
libra add tracked.txt
libra commit -m "initial" --no-verify
libra --json status
libra log --oneline
libra log -n 1 --name-status --grep initial --author "Libra Integration"
libra log --stat -n 3
libra fsck --connectivity-only

mv tracked.txt renamed.txt
libra add renamed.txt
libra rm --cached tracked.txt
libra status --short
libra status --porcelain v2
libra status --porcelain -z
libra status -z -s
libra --json status
libra commit -m "rename tracked" --no-verify
libra log --oneline renamed.txt
libra log --follow --oneline renamed.txt
libra log --name-status renamed.txt
libra --json log renamed.txt

mkdir -p scratch
printf 'untracked\n' > scratch/note.txt
libra status --short --untracked-files=no
libra status --short --untracked-files=all
libra status --short --branch
```

关键断言：

- JSON `status` / `log` envelope 可解析。
- 空提交必须失败且不移动 HEAD；`status -z` 必须输出 NUL 结尾记录；--follow 现在应成功。
- rename 当前通过 `A renamed.txt` + `D tracked.txt` 观察，不断言 rename-follow 历史。
- Unix 下 symlink typechange 通过 porcelain v2 mode `120000` 和路径观察。
